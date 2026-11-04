# Design: generic instances shared by shape (Go-style dictionaries)

Status: draft for review (#2271, part of #2189 and #1612 item 2). It starts from the proposal on
#2189 (2026-11-03 17:19) and answers the five points #2271 leaves to review. Related designs:
[package-cache.md](package-cache.md) (the per-package cache, #2236), #375's generic interface
methods (implemented in #943 as per-instantiation slot tables, `lower/glue/generic_slots.rs`),
and `AnyUnbox`, the out-of-line slow path of `any as T` (#2268). Nothing here is implemented.

## Problem

Velt compiles a generic function once per list of type arguments. On sigx small-page, 61% of
the release IR (about 22M lines) is std generic instances. The largest families are:
- `Map` methods, 3.62M lines;
- the array-view sync helpers, 3.32M;
- `mapCompact`, 0.99M;
- `Set`, 0.91M;
- `Record`, 0.69M;
- `mapResize`, `mapKeys` and related helpers, 0.86M.

Stubbing these instances in one codegen unit cut its clang time by 64% (#2189, "What the release
IR is made of"). Most of that code doesn't depend on the type argument. The rest of it
depends on a few of the type's operations.

The prototype target shows the pattern clearly (#2271, first comment). In a ten-line program
that converts an `any` to `Item[]`, `arrayViewSync<Item>` is 37,862 release IR lines (13,569
VIR lines) and `arrayViewSync<string>` is 3,404. The helper's own loop is a few hundred lines:
- most of the rest is `cur as T` written out per element type;
- 12% is the cold `TypeError` paths.

#2268 moves the `as T` slow path into one `AnyUnbox` glue function per target type. What it
leaves is the helper's body, compiled once per element type, with nothing in it that differs
between those types except calls.

Go compiles one copy per *GC shape* (a type's size, alignment and pointer layout) and passes a
dictionary of the type-specific operations. Rust compiles one copy per type, which is a large
part of its build cost. The owner chose Go's model (#2189, "Architecture direction", item 2).
This note fixes the details for Velt, where two things differ from Go:
- **Ownership is part of the representation.** A class object is either counted (a count word
  in front of it) or owned by one reference, depending on what the whole program does
  (`lower/boxing/`). The two need different inline code for the same 8-byte pointer.
- **Shapes exist only during lowering.** Sema and HIR stay as they are; this is a lowering
  decision, plus one `velt_opt` change for release.

## 1. The shape key

A *shape* is everything about a type argument that a shared instance's code may depend on
without asking a dictionary. Two type arguments with the same shape get the same code. Every
other difference goes through the dictionary (section 2).

### The classes

Every shape has a class, and a class determines everything that is compiled inline:
- the VIR representation (`Cx::ty`);
- whether `T | null` uses the null pointer (`has_null_niche`);
- how a copy, a drop and `===` are compiled when they are compiled inline.

| shape | VIR | types | compiled inline | through the dictionary |
|---|---|---|---|---|
| `~i8` … `~u64`, `~f32`, `~f64`, `~bool`, `~v128` | the scalar | numbers of that type, `bool`, C-like enums (`I64`), storage floats (`U16`), SIMD | everything: copy, `===`, no drop | format, `any` table, from `any`, hash |
| `~sym` | `Ptr`, never null | `symbol` | copy, `===` (identity) | format, `any` table |
| `~rc` | `Ptr`, null niche | counted class objects and counted boxes (arrays, object types) outside the lock set | count increment and decrement, `===` (identity), move | drop when the count reaches zero, format, clone, transfer, trace, `any` table, from `any`, structural eq and hash |
| `~rc.lk` | `Ptr`, null niche | as `~rc`, in the lock set (`boxing/locks.rs`) | as `~rc`, through `velt_rt_lk_*` | as `~rc`, plus the type word the `_of` variants take |
| `~own` | `Ptr`, null niche | uncounted class objects (one owner) | move | drop (frees), format, clone, transfer, … |
| `~shared` | `Ptr`, null niche | `shared<T>` | move | share (its atomic count), drop, format |
| `~promise` | `Ptr`, no niche | `Promise<T, E>` | move | drop, format |
| `~str` | `Agg(STR_AGG)` | `string` | everything string code does today | format (quoted inside containers) |
| `~arr` | `Agg(array_agg)` | uncounted arrays | header moves, `length` | drop, clone and format (each walks the elements), `any` table, from `any` |
| `~fn` | `Agg(closure_agg)` | function values | share and drop (through the environment's own header) | format, `any` table, `===` when the program compares functions |
| `~dyn` | `Agg(dyn_agg)` | interface values | share, drop and format (through the vtable) | `===` (`identity_dyns`), `any` table |
| `~unit` | `Unit` | `void`, `never`, literal types | nothing, since the value has no bits | format, `any` table |
| `~agg(s, a)` | `Agg` of size *s*, alignment *a* | value types that own parts: object types never changed in place, tuples, unions, `T \| null` without a niche, `Result` | move (a copy of *s* bytes) | share, drop, format, eq, hash, `any` table, from `any`, … |
| `~agg.plain(s, a)` | as `~agg` | as `~agg`, owning nothing (`ShareKind::Plain`) | move, share (both a copy), no drop | format, eq, hash, `any` table, from `any`, … |

**Aggregates.** Value types are keyed by size and alignment only. Every aggregate operation
in shared code is either a copy of the whole value or goes through the dictionary. Lowering
copies aggregates with `memcpy` (`Operand::Copy` of an `Agg` place), passes them by pointer and
returns them through an out-pointer. Shared code never reads one of their fields, tags or
presence flags. So two aggregates of one size and alignment run the same code whatever their
fields are. Go needs the pointer layout too, for its collector; Velt doesn't. Derived types
follow: `T | undefined` of an aggregate is `{ some: bool, value: T }` (`value_agg`), and an
array's stride is the size rounded to the alignment, so both depend only on *s* and *a*. Inside
a shared instance, a value of `~agg(s, a)` has a lowering-made `AggLayout` of *s* bytes in words
of *a*: one per shape, not one per type.

Aggregates are in the first step because they are the prototype's case. In #2271's repro, `Item`
(an object type) is `~agg(64, 8)`, and sigx's `JSXChild` (`VNode | string | number | … |
JSXChild[]`) is a union, also an aggregate. A prototype limited to pointers and scalars would
share nothing there.

### Point 1 of #2271

**Is `Map<string, A>` the same shape as `Map<string, B>` when A and B are both counted
pointers?** Yes. Both are `Map<~str, ~rc>`, with one copy of each method, provided:
- **A and B are both counted.** If A is counted and B has one owner, `Map<~str, ~rc>` and
  `Map<~str, ~own>` are two copies, because a count decrement is inline code and a single
  owner's drop is a call. `Map<string, number>` is `Map<~str, ~f64>`, a third copy.
- **Neither is in the lock set,** whose count operations are runtime calls (`~rc.lk`).

**Why the key has no separate "trivial equality, hash and drop" bits.** For every class above,
those facts already follow from the class:
- scalars, `~sym` and `~unit` never drop;
- `===` is bitwise or identity for every class except `~str`, `~fn` and `~dyn`, and those have
  their own code (`~str` is one type);
- structural `Eq` and `Hash` are calls in per-type code too.

The one exception is aggregates: an aggregate that owns nothing copies and drops with no code at
all, so `~agg.plain` is its own shape. Equality and hash of aggregates go through the
dictionary in both cases. Bitwise comparison would be wrong for floats (`NaN`, `-0`) and
padding, and per-type code calls `Eq` and `Hash` glue for them too.

**The key covers the derived types too.** The counted-types passes decide per `TyId`: whether
an array type or an object type is a counted box is in `Boxing::boxes`, keyed by the type, so
`A[]` can be counted while `B[]` is not. The helpers store a `T[]` in `ArrayView<T>`, so the shape
of `T` alone doesn't fix their code. The key of an instance is therefore the shapes of its
*footprint*: every type in the body that mentions a type parameter (`T`, `T[]`,
`T | undefined`, `ArrayView<T>`, a callee's type arguments). The footprint is collected once per
definition by the scan below. Two lists of type arguments share an instance when every
footprint type has the same shape under both. Usually that is the same as "the parameters have
the same shapes". When it isn't, the result is two instances, never wrong code. Class
hierarchies are counted per root definition (`Boxing::roots`), so `ArrayView<A>` and
`ArrayView<B>` always agree.

**Shapes are per type parameter.** Each type argument is a shape or stays concrete. Debug
replaces every eligible one. Release may keep an argument concrete when its operations are hot
and small (section 3). Example: `Map<string, V>`'s hashing of the key stays inline while `V`
is shared.

### Shapes and the counted-types fixpoint

Whether a class is `~rc` or `~own` is decided by lowering's counted-types passes (`Boxing`), so
a shape is computed per pass, from that pass's `Boxing`, as layouts already are. A type that
becomes counted in pass N changes shape in pass N. The instance it uses changes with it, and
the passes still converge, because the facts are the same as before (next paragraph).

A per-type instance records the facts that drive the fixpoint for its own type arguments: a
share of `T` asks the next pass to count `T` (`note_share`), and a borrow inside a counted
object becomes `note_projection`. A shared instance records them in terms of its parameters,
for example "parameter 0 is shared". Each dictionary made for the instance replays those facts
on its concrete arguments, and so does each fact found after the dictionary exists, with the
same growing-table bookkeeping as section 2. The result is the set of facts the per-type
instances would have recorded. The prototype checks that in a test mode
(`VELT_SHAPES=check`): it lowers each eligible instance both ways and compares the final
`Boxing`.

This touches the passes' bookkeeping (`lower/boxing/`), which the lowering workstream owns. #2221
(lower once, facts-only passes) doesn't change it: the facts are recorded the same way, by
fewer passes.

### How lowering represents a shape

The instance key stays `Work::Fn(def, targs)`. For a shared instance, each shared argument in
`targs` is a *shape placeholder*: a `TyKind::Param` with an index above `SHAPE_BASE`, interned
by lowering, so it is a `TyId` like any other. HIR never contains one. This gives
three properties without touching the HIR contract:
- **Memo tables stay correct.** `drop_memo`, `value_agg`, vtable and glue keys are keyed by
  `TyId`. A placeholder is interned per instance key and parameter, so it and every type built
  from it (`Array(placeholder)`) always denote the same footprint shapes. The shape queries on
  such a derived type read the instance's footprint.
- **`FnLower::sub` is the one place where instance types enter lowering.** It substitutes the
  placeholders exactly as it substitutes concrete arguments today.
- **Leaks fail loudly.** Many lowering matches end in a wildcard (`share_kind`'s `_ =>
  ShareKind::Plain`), so a placeholder reaching them would silently get a wrong answer. To
  prevent that:
  - `Cx::kind` ICEs on a placeholder;
  - the shape queries (`ty`, `has_null_niche`, `share_kind`, `needs_drop`, `boxed`) answer for
    a placeholder from the footprint before they would call it;
  - the few reads of `types.kind` that bypass `Cx::kind` (`is_union`, `has_null_niche`, …) get
    the same check.

  So every per-type question asked of a placeholder is an ICE in the test suite, not a wrong
  value: `type_name`, class ids, vtables, `any_target_name`. Everything else goes through the
  dictionary.

A generic definition's body is scanned once (HIR, memoized per `DefId`) to decide which of its
type parameters can be shared. A parameter can be shared when every use of a value of a type
mentioning it is one of these:
- a move, copy, borrow, drop or share;
- `===` or `!==`;
- a field or element of a generic class or array, whose layout follows from the shape;
- a conversion to or from `any` or `unknown`;
- `console.log`, `String()` or a template;
- a call of a generic function or method, through a sub-dictionary or a code pointer;
- a conversion to an interface (`T as I`);
- a call of a generic interface method on an interface value (#375);
- an intrinsic that answers a type question at compile time (`HoldsDynamic`,
  `MayHoldPrimitive`, `needsDrop`, `fnCapturesNothing`, …), which becomes a dictionary word
  that the branch tests.

It cannot be shared, and the definition keeps per-type instances for it, when:
- a deferred type operator (`TyKind::Op`) mentions it;
- generated code is per type (JSON readers and writers, `generate/`);
- it crosses a foreign boundary (`extern`, native packages).

In the first steps, async functions, generators, and generic closures that use dictionary
operations are also kept per type.

The scan is conservative, and the ICE is the backstop. A body the scan accepts that still asks
a per-type question is a bug in the scan, and a golden run with sharing on everywhere finds it
(section 4).

## 2. The dictionary ABI

There are two kinds of read-only statics, both `StaticData` with `mutable: None` and relocations,
exactly like vtables. So no VIR type, verifier rule or backend changes.

### Type tables

A **type table** (`_Gtype` *T*) is one per concrete type that reaches shared code as a type
argument. It uses the vtable layout (`glue/vtable.rs`):
- the seven hidden slots, with the same constants: class id, transfer, name, share, format,
  clone, drop;
- then the dictionary slots:

| slot | entry | signature (as the glue's) |
|---|---|---|
| −7 | class id (a word) | |
| −6 | `Transfer` | `(p)` |
| −5 | the type's name, a static string (debuggers, section 4) | |
| −4 | `Share` | `(src, dst)` |
| −3 | `Format` | `(buf, p, depth)` |
| −2 | `Clone` | `(src, dst)` |
| −1 | `Drop` (`~rc`: `ObjDrop` / `QueuedDrop`, called when the count reaches zero) | `(p)` |
| 0 | `Eq` | `(a, b) -> bool` |
| 1 | `Same` | `(a, b) -> bool` |
| 2 | `SameValue` | `(a, b) -> bool` |
| 3 | `KeySame` | `(a, b) -> bool` |
| 4 | `Hash` | `(p) -> u64` |
| 5 | the `any` table of *T*: its descriptor, which `any as T` compares and `ToDyn` stores | |
| 6 | from `any`: `AnyUnbox` (#2268), or `AnyNumericOrError` for numbers | `(data, desc, out)` |
| 7 | `typeof` tag (a word) | |
| 8 | `Trace` / `TraceIn` | `(p, visit, ctx)` |
| 9 | `JsText` | `(p, buf)` |
| 10 | the lock-set type word for `velt_rt_lk_*_of` (`~rc.lk` only) | |

A slot is filled only when a shared instance reads it. A slot nothing reads stays null, and no
glue is generated for it. This is the rule #2249 applies to vtables' format, clone and transfer
slots. Type tables don't keep unused glue alive, and they never pull in the `any` member glue,
which is 25% of small-page's IR: slot 5 points at the `any` table only when shared code converts
*T* to `any`.

### Instance dictionaries

An **instance dictionary** (`_Gdict` *f*`<`*targs*`>`) is one per generic definition and list of
concrete type arguments that calls a shared instance. It is an array of words, in the order the
shared instance's lowering asks for them:
- the type table of each shared type argument, and of each *derived* type the body needs as a
  type argument or a conversion target (`T[]`, `T | undefined`, `ArrayView<T>`);
- a **sub-dictionary** for each generic callee whose arguments mention a shared parameter
  (Go's nested dictionaries). When the callee is itself shared, the entry points at the
  callee's dictionary for the substituted arguments, and the call passes it. When the callee
  is not eligible, the entry is the address of the callee's per-type instance, which the
  shared code calls. So a shared function can always call a callee that stays per type;
- the vtable of each `T as I` conversion;
- the slot index of each generic interface method called with type arguments that mention a
  shared parameter (#375, next section);
- the words of the compile-time type predicates (`HoldsDynamic`, …).

The layout is found while the shared instance is lowered, so dictionaries grow as entries are
found. This is the problem `generic_slots.rs` already solves for #375's slot tables: a list of
keys per table owner, each new key appended to every table made so far, and each table made
later starting with every key known by then. The statics are emitted once every function is
lowered. The prototype generalizes that code, `GenericSlot` becoming a growing-table type used
by both, rather than writing a second copy.

### The hidden parameter

The dictionary is a hidden `Ptr` parameter of the shared instance. It comes after the user's
parameters and before the trailing out-pointer and the caller's location (`srcloc::ARG_LOC`).
That way:
- the hidden parameters keep their order;
- the closure environment stays first;
- the masks and attributes that index user parameters (`frozen.rs`, `param_attrs`) don't shift.

Its `ParamAttrs` are read-only and `noalias`, so LLVM may hoist dictionary loads out of
loops.

- **Callers.** Monomorphic code passes `Operand::Const(Const::Static(dict), Ty::Ptr)`. Shared
  code passes a sub-dictionary loaded from its own dictionary.
- **Function values.** A shared instance used as a function value, or reached from a vtable
  slot, goes through a thunk that passes the constant dictionary. There is one thunk per
  `(def, targs)` (`Work::Thunk`, as the borrow-ABI adapters are). So function identity is the
  same as today: `f<A>` and `f<B>` as values are different functions, and `f<A>` taken twice is
  one.

**Rejected: a dictionary field in the receiver.** Swift and C# keep the type information in the
object. It would cost every `Map` and `Set` object a header word and change the class layout.
It would also cover only methods: `arrayViewSync<T>` is a free function, a method's own type
parameters (`map<U>`) aren't the receiver's, and a constructor makes its receiver. A hidden
parameter covers all of these with one rule.

### One mechanism with #375

#375 decided per-instantiation vtable slots (owner, 2026-11-02): no boxing and no hidden
dictionary on interface calls. #943 implemented them as generic slot tables. Shape sharing
keeps that decision and meets it in three places:
1. **The same kind of static.** Vtables, generic slot tables, type tables and instance
   dictionaries are all read-only statics of relocated words, read with a constant offset and
   called (`dispatch.rs`). The hidden slots use the same constants (`SLOT_DROP` …). There is one
   growth algorithm, `generic_slots.rs` generalized. There is one fold in `velt_opt`, below.
2. **Generic interface calls from shared code.** `x.wrap<T>()` on an interface value, inside an
   instance where `T` is shared, needs the index of `[T]` in the slot's tables. That index is a
   constant per concrete `T`, so it is a dictionary word. The call loads the instantiation
   table from the vtable (as today) and the index from the dictionary.
3. **Shared implementations in slot tables.** A slot table entry of a shared method is the
   method's thunk with its constant dictionary, as for any function value. Interface dispatch
   costs what it costs today. The only extra is the thunk's tail call where the method is
   shared, and release removes it (section 3).

If #375 is ever switched to dictionary passing, that is the same hidden parameter: the slot's
code takes a dictionary built from the slot's type arguments.

### velt_opt sees through a constant dictionary

`vtable_loads` folds a load of a relocated slot of a read-only static, through a local that holds
`static + offset` on every path, into the relocation's target. `constfold` then makes the call
direct, and the next inlining round can inline it. A dictionary is such a static. So:
- **Inlined shared instances need nothing new.** When the inliner moves a shared instance into a
  caller that passed a constant dictionary, the dictionary parameter becomes a local holding
  the static. Its loads fold: a sub-dictionary or type table becomes a `Const::Static`, which
  the next run of the pass folds again.
- **One change, for the instances that aren't inlined.** `const_fields/specialize.rs` already
  clones a callee for a read-only parameter whose fields are known: a closure's code pointer,
  which is how `sort(cmp)` gets a direct comparator. Extending "known" to "points at a read-only
  static" clones the shared instance with its dictionary folded in, which is today's per-type
  code, at the call sites that need it. Its `MAX_CLONE_COST` and `MAX_CLONES` bound the
  growth. This is release only (section 3), and the prototype doesn't need it.

Step 3 adds a `velt_opt` unit test for the two-level fold (dictionary, then type table, then
function), with the release workstream that owns `velt_opt`'s inliner. `addrs.rs` treats
parameters as unknown, so the fold applies once the instance is inlined or specialized, which is
when it is needed.

### The per-package cache and facts traces

A shared instance refers to no per-type static. Every type-specific thing it touches arrives
through its dictionary parameter. Its facts trace (package-cache.md) is therefore its shapes:
the answers it reads (is `T` counted? in the lock set?) are the shape key itself. So shared std
instances are the entries that repeat between programs, and they are what std can ship compiled
for the common shapes (`~rc`, `~f64`, `~str`, `~i32`, `~bool`).

Dictionaries and type tables are per program. They are the "statics with symbols" of
package-cache.md step 2: the cached caller of a shared instance refers to its dictionary by
symbol, and the program defines it. `PackageCode::lookup(func, args, facts)` is asked with the
placeholder arguments. The cache keys them by shape name, as symbols do, never by `TyId`, so the
interface stays as package-cache.md has it.

## 3. Debug and release

**Both modes use one key and one scan.** A type argument is shared or not for the same reasons in
both, so the goldens test one semantics. Release adds profitability, not eligibility.

**Debug shares everywhere.** Every eligible instance in std and user code shares, for every
shape. Debug builds run only `velt_opt`'s cheap passes, and the glue is a call in both versions,
so the cost is an indirect call and a dictionary load where today's code has a direct call. Run
time of debug builds isn't gated. Their build time and memory are. This is #1612's "debug builds
without per-type copies".

**Release shares when it doesn't cost run time,** with run time ±3% as the gate (the perf rule).
Three rules decide it. The first two are local and cheap to compute:

1. **No inlining lost.** While a shared instance is lowered, each dictionary read is classified
   as **call** or **inline**. A *call* is an operation the per-type instance also compiles as a
   call: a drop when the count reaches zero, clone, format, transfer, the `AnyUnbox` slow path,
   a call of a sub-dictionary's callee. An *inline* read is one the per-type instance would have
   compiled inline: a type predicate turned into a branch, a descriptor compared on the fast
   path of `as T`, the share or drop of a small `~agg` (its parts' counts, written out). An
   instance with only call reads is shared in release. Only a call through a pointer is lost,
   and the callee is out of line in both versions. For the classes of section
   1, almost every read is a call. The exceptions are the fast-path descriptor, which costs one
   load that LLVM hoists out of the loop, and the share and drop of aggregates, which rule 2
   covers.
2. **Hot sites get a per-type copy back.** An instance with inline reads is shared too, and
   `velt_opt`'s specializer (section 2) clones it with the constant dictionary at a call site in
   a loop when the callee is under its size limit. A shared instance that only one dictionary
   ever reaches is specialized at every site and the shared original is deleted (`dead_funcs`),
   so a program with one `Map` type pays nothing.
3. **The gate decides, per family.** A family (the array-view helpers, `Map` methods, `Set`,
   `Record`, user generics) is shared in release only once:
   - every benchmark of `bench/nightly.sh` and the sigx scenarios is within ±3% by cachegrind
     instructions (`bench/compiler_ab.sh BASE HEAD`);
   - the hot ones (`bench/array_view`, `bench/btree`, `bench/concurrent_map`, `bench/json`) are
     also interleaved best-of-N by wall time.

   Until then, the family stays per type in release, by a per-family switch in lowering
   (`shape::RELEASE_FAMILIES`). A family that fails the gate is fixed (a concrete argument
   kept, a predicate left inline) or stays per type.

So what decides is rule 1, computed by lowering; rule 2, computed by `velt_opt` from call sites
and size; and the measured gate, kept as a list in the code that only grows. A switch in
`VELT_SHAPES` (`off`, `on`, `check`, `all`) picks the behavior for A/Bs without rebuilding the
compiler.

Release is unchanged in step 2 (order of work): it shares nothing, and its VIR is
byte-identical to main's for every golden and benchmark program, which the step checks.

## 4. Semantics: zero observable difference

**The invariant.** Shared code never answers a per-type question itself. Everything whose answer
depends on more than the shape is one of three things:
- a dictionary read of code built per type, by the same glue builders as today;
- a constant computed per type into a dictionary word;
- a placeholder query that ICEs.

So a value's observable behavior is produced by the same code, in the same order, as in a
per-type instance. The difference is only how the code is reached.

| observable | where it comes from in a shared instance |
|---|---|
| `console.log`, `String()`, templates | `Format` and `JsText` of the type table: the glue per type, so class names, quoting and nesting are those of the type |
| `TypeError` from `x as T` | the fast path compares the descriptor from slot 5. Otherwise `AnyUnbox` of *T* (slot 6) builds the message, with *T*'s name, as #2268's glue does. A site whose message names what it converts (`any_context`) keeps the per-type instance in the first steps (#2268 keeps those sites written out as well) |
| other errors and panics | messages are per type only through glue. Locations are the source locations of the body, the same in every instance, and caller-tracking passes its location parameter on as today |
| `instanceof` | class ids are in object headers and class ranges are per class definition, so neither depends on which code tests them. `instanceof` a type parameter is not TypeScript |
| `typeof`, `Array.isArray`, truthiness of a `T` | the `typeof` tag word, the `any` table, or the shape (`~f64` is always `"number"`) |
| `===`, `Object.is`, `Map`/`Set` keys | inline where the class makes it bitwise or identity, otherwise `Same`, `SameValue`, `KeySame` and `Hash` of the type table. Hash values are the same, so iteration order is too |
| drop order, `dispose`, weak references | the same drop and trace glue, called at the same points of the same body |
| function identity | one thunk per `(def, targs)`, so as today (section 2) |
| threads (`transfer`, `shared`) | `Transfer` and `ManyCheck` per type |
| `e.stack` | Velt keeps no call frames, only the first line (`tests/golden/lang/error_stack.vlt`), so this is unchanged |
| stack traces in debuggers, profilers and crash dumps | see below |

**Stack traces.** A shared instance's symbol names its shape: `arrayViewSync_T` followed by a
shape segment, demangled `arrayViewSync<~rc>`. Like other symbols it contains no `TyId`, so
`velt dev` matches it across versions. Debuggers and profilers show that name, which is correct
code-wise but doesn't say which type was meant. Debug builds name the types too:
- each type table holds its type's name (slot −5);
- the dictionary parameter is described to the debugger as a variable `$types`, listing the
  type arguments by name through the debug-variable support (`debug-variables.md`).

A frame then shows `arrayViewSync<~rc>` with `$types = [Item]`. The prototype checks this with a
case in `crates/veltc/tests/debugger.rs`, next to the existing debug-variable cases. Release strips
symbols at link time, as it does today. Panic messages carry source locations, not function
names, and are unaffected.

**How it is tested:**
- **The bars.** Every golden passes, with output byte-identical between debug, where sharing is
  on, and release, where it is off.
- **`VELT_SHAPES=all`.** Shares every instance the scan accepts, including the families release
  doesn't share yet, and runs the whole golden suite in debug and release. This runs in CI as
  one job, so each leak or scan bug shows up as an ICE or a wrong output, not in a user's
  program.
- **`VELT_SHAPES=check`.** Compares the counted-types facts both ways (section 1).
- **New goldens, each run with Node,** for what crosses a shared instance:
  - a view of class objects, counted and single-owner, printed and converted wrongly (the
    `TypeError` text);
  - `Map` and `Set` keys of several class types;
  - a shared method reached through an interface slot;
  - a generic method called through an interface from shared code;
  - function values of instances compared.
- **Unit tests in `velt_vir`.** The shape of each type kind, and a placeholder's per-type query
  ICEing. Each test is checked to fail with the key broken on purpose: `~rc` and `~own` merged,
  or the niche bit dropped.

## 5. Measurement plan

Every step reports base against head, on the same machine:
- builds run with a fresh `VELT_HOME` and `VELT_BUILD_CACHE=off VELT_BUILD_STAMP=off`;
- instructions are counted with cachegrind over `velt` and every child process
  (`--trace-children=yes`). On a busy machine they are the numbers to compare;
- wall time is interleaved, best of five;
- peak RSS comes from `/usr/bin/time`.

| program | what | modes |
|---|---|---|
| the `Item[]` repro (#2271) and `bench/array_view` | instances of the helpers: VIR and LLVM IR lines (`--emit vir`, `--emit llvm`), function count | debug, release |
| hello | that nothing changes for a program without views (instructions, IR lines) | debug, release |
| `bench/compile/scale/` 10k and 100k | IR lines, build CPU and instructions, wall, peak memory; `velt build -v` stage times (lower, optimize, codegen) | debug, release |
| sigx small-page (on the builder; it isn't in this repository) | the same, plus `VELT_LOWER_PROFILE` blocks per pass, and both renders against Node | debug, release |
| the nightly set, `bench/array_view`, `bench/btree`, `bench/concurrent_map`, `bench/json` | run time: cachegrind instructions ±3%, wall interleaved best of N for the hot ones | release (and debug, reported, not gated) |
| goldens | debug and release outputs byte-identical; release VIR byte-identical to base in steps where release doesn't change | both |

New counters in `VELT_LOWER_PROFILE`:
- shared instances;
- per-type instances they replace;
- dictionaries and type tables, with their words;
- per family: instances before and after;
- the instances the scan rejected, by reason, so the next family's work starts from data.

Expected effect, to check against these numbers: the helpers' instances drop to one per shape
used. That is one to three on small-page (`~rc` for JSX nodes, `~str`, maybe `~f64`), against
one per element type today. Their share of the IR goes from 3.32M lines toward the size of
those few instances plus the dictionaries (a few words per type). The `as T` slow paths were
already moved out by #2268, and they move into the type tables' `AnyUnbox` entries unchanged.

## Order of work

1. **This note.**
2. **Prototype, debug only, the array-view sync helpers.** It covers `arrayViewSync`,
   `arrayViewSyncAt`, `arrayViewSyncLen`, `arrayViewFlush`, `arrayViewFlushFrom`,
   `arrayViewFlushAt` and `arrayViewLen` (not `arrayViewSort`, whose closure and comparator
   wait for closures). The classes are the scalars, `~rc`, `~own`, `~str`, `~arr` and `~agg`. It
   includes:
   - the placeholder types;
   - the type tables;
   - the instance dictionaries on the generalized growing tables;
   - the hidden parameter;
   - the facts replay;
   - `VELT_SHAPES`;
   - the counters.

   The bars:
   - all goldens pass, with debug and release output byte-identical;
   - IR lines, build CPU and peak memory for small-page and scale 10k/100k, base against head;
   - release unchanged.

   It builds on #2268 (`AnyUnbox`) for slot 6.
3. **Release for the helpers.** Rules 1 and 2, the `velt_opt` specializer extension, and the
   gate.
4. **`Map`, `Set`, `Record` and `mapCompact`/`mapResize`/`mapKeys`, then every eligible std
   generic, then user generics.** Debug first, then release family by family through the gate.
   Generic closures (the dictionary as an implicit capture), async functions and generators
   come in this step.
5. **Glue shared by shape:** the drop, clone and format of `Map<K, V>` objects themselves, and
   of arrays of aggregates, with the element glue from the type table.
6. **Std ships compiled shape instances** (package-cache.md layers 2 and 3).

## Who owns what step 2 touches

Three other workstreams own parts of `velt_vir` lowering (#2189, 2026-11-03 17:09). Before step 2
edits their files, it says what and why on #2189:

| area | owner | what step 2 needs there |
|---|---|---|
| the passes and the counted-types fixpoint (`lower/boxing/`, `view_unswitch*`, `spill/`) | lowering | recording facts per parameter and replaying them per dictionary (section 1). A hook at `note_share` and `note_projection`, no change to how the set is closed |
| `any` operations (`any_*.rs` sites, `any_unbox`) | `any` operations | the fast path of `as T` reading the descriptor from slot 5 and the slow path calling slot 6 when the target is a placeholder: one branch at the top of `any_unbox` |
| glue and vtables (`lower/glue/`, `lower/any.rs` tables) | release backend and glue | type tables built from the existing glue builders; `generic_slots.rs` generalized into the growing table both use |

Step 2's own code is a new `lower/shape/` module with the key, the scan, the type tables and
dictionaries. It also touches the instance key at call sites (`call.rs`, `callee.rs`), the
hidden parameter in `abi.rs`, and `Cx::ty`'s placeholder case in `types.rs`. `ast.rs`, HIR,
`vir.rs` and the contract documents don't change. If the placeholder convention
(`TyKind::Param` above `SHAPE_BASE`, lowering only) should be written down, it goes into
`docs/internals/contracts/hir_encodings.md` with the maintainer's review.

## Open questions

1. **`~own` and `~rc` in debug.** Debug could merge them into one `~ptr` shape, with the count
   operations in the dictionary, for fewer instances at a call per share. I propose keeping them
   apart in both modes, so one key is tested, unless step 2's counters show programs where the
   split doubles many instances.
2. **The site text of `as T`.** `AnyTo` with a text (`any_context`) keeps its per-type instance
   in the first steps, as #2268 keeps such sites written out. Passing the text to `AnyUnbox` as
   a fourth argument would let those sites share too. That decision belongs to #2268's owner.
3. **Type tables for `velt dev`.** Hot reload matches functions by symbol, and the shape
   symbols are stable. Dictionaries are data rebuilt per version. Step 2 adds a `velt dev` test
   with a view across a reload, to confirm that a reloaded caller passes the new version's
   dictionary.
