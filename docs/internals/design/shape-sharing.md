# Design: generic instances shared by shape (Go-style dictionaries)

Status: step 2 is implemented (#2285), in debug builds only; steps 3 and later are proposed
(#2271, part of #2189 and #1612 item 2). It starts from the proposal on
#2189 (2026-11-03 17:19) and answers the five points #2271 leaves to review. Related designs:
[package-cache.md](package-cache.md) (the per-package cache, #2236), #375's generic interface
methods (implemented in #943 as per-instantiation slot tables, `lower/glue/generic_slots.rs`),
and `AnyUnbox`, the out-of-line slow path of `any as T` (#2268).

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
without asking a dictionary. Two lists of type arguments get the same code only when every
such answer is the same for both. Every other difference goes through the dictionary (section
2).

### The key is a list of answers, not a table of classes

Lowering compiles code inline from per-type facts in many places, and a table of classes
misses some of them:
- the weak-capable release (`rc.rs` `release_as`);
- union tags, which follow `TyId` order (`canon_union`);
- parameter attributes (`unique_refs`, `holds_mutex`, the object size behind
  `dereferenceable`);
- lock-set reach, transfers and array views;
- nullable pointers.

So the key is defined by a closed list of **shape questions**: per-type queries that inline
code may depend on. Shared code may ask only these about a placeholder, and the key is their
answers:

| question | `Cx` query | why inline code depends on it |
|---|---|---|
| layout | `ty` (an aggregate as its size and alignment) | moves, locals, strides |
| share kind | `share_kind` | the copy written at a share |
| needs drop | `needs_drop` | whether a drop is emitted; for a function value it varies per closure type (`closure_env_is_heap`, `fn_identity`), so a `Closure(d)` that reaches a type argument splits by it |
| null niche | `has_null_niche` | `T \| null` as a pointer |
| class object | `is_class` | parameter shape, `PtrParam::Object` |
| counted | `counted`, `boxed` | count increment and decrement inline |
| interior | `unique_refs` | `noalias` (vir.rs invariant 9) |
| lock set | `lock_counted`, `reaches_lock_set`, `shared_reads` | `velt_rt_lk_*` counting, read-mode pinning at stores and calls (`caller_stores.rs`, `caller_aliases.rs`, `leave_levels.rs`, `hand_result.rs`) |
| weak | `weak_capable` | the three-way release (`rc.rs` `release_as`) |
| transfers | `transfers` | what a hand-over to another task writes |
| mutex | `holds_mutex` | `frozen` |
| proxy | `proxiable` | typed-proxy tests at reads and drops |
| array view | `array_view` of `T[]` | the view sync at every access (`sequence.rs`, `array_view_ops.rs`, `view_unswitch*`) |
| union | `is_union` | truthiness and narrowing of the value |

The classes are just names for common answer lists:
- `~rc`, `~own`, `~rc.lk`, `~rc.weak`;
- `~rc?`, `~own?`, `~sym?` for a nullable pointer, which uses the niche: its share and
  drop test for null first, and `(T | null) | undefined` can't use the niche again;
- `~f64`, `~str`, `~arr`, `~agg(64, 8)`, …

A new inline dependency is one more row, not a new class.

**Point 1 of #2271.** Is `Map<string, A>` the same instance as
`Map<string, B>` when A and B are both counted? Yes, when every row above gives the same
answer for both, and so for their footprint types (below). A weak-capable A against a plain B,
or an A in the lock set, splits them. `Map<string, number>` is another instance in any
case.

### The footprint

The questions are asked of more than the type argument itself. A definition's **footprint** is
every type in its body that mentions one of its type parameters: `T`, `T[]`, `T | undefined`,
`ArrayView<T>`, a callee's type arguments.
- **The key.** The key of a type argument is the answers to the shape questions, plus its
  layout and membership in each counted-types set, for the type itself and for every type of
  the footprint. In step 2 every shared definition is one of the array-view helpers, whose
  key covers `T` and `T[]`. The helpers also ask about `ArrayView<T>`, which the key leaves
  out: no program is known where it differs while `T[]` agrees, and the end-of-pass check
  would catch one. A view of `T[]` or of `T | null` is the helpers instantiated at that type,
  with its own key. The families of step 4 build other types from their parameters: there the
  footprint is collected per definition by a HIR scan (memoized per `DefId`), and the key
  covers it.
- **Placeholders are per key and parameter position.** Two definitions share a placeholder
  only when the key, which covers the footprint, is the same. So every type built from the
  placeholder has the same answers for every member, and the global memo tables keyed by
  `TyId` (`drop_memo`, `value_agg`) can't mix two footprints. Once footprints are per
  definition (step 4), the key includes the definition.
- **The end-of-pass check.** At the end of the pass, the placeholder and every type built from
  it that shared code asked about are instantiated with every member of the shape. Their
  answers are compared with the representative's, from the same list the key is made of. A
  difference is an ICE: the footprint missed a type. So the check never fires on a program
  the key describes, and a key too coarse is caught, not compiled.

### How the questions are enforced

`Cx::kind` ICEing for a placeholder isn't enough on its own. Most per-type facts are reads of
`Boxing`'s sets (`self.boxing.<set>.contains(&t)`, 16 sites), and other code in `lower/` matched
on `types.kind` directly (52 reads, which now go through `Cx::raw_kind`). All of these answer
silently for a placeholder or an `Array(placeholder)`. Step 2 therefore delivers three things,
each with a test that breaks it on purpose:
1. **The shape queries answer for the representative.** A placeholder has a representative,
   the first concrete type of its shape. Each shape query maps a placeholder to its
   representative first (`Cx::ph_rep`). Since the key makes every member give the
   representative's answers, that answer is right for all of them.
2. **`Boxing`'s sets are read through the representative.** The accessors that read a set of
   types map a type built from a placeholder to the same type built from the representative
   (`Cx::ph_rep`) and ask about that. Nothing is put into or taken out of `Boxing`. A test
   fails when code outside `boxing/` reads a set of types without the mapping.
3. **Every other question about a placeholder is an ICE.** That covers `Cx::kind`. Every read of
   lowering's type table that matches on a kind directly goes through `Cx::raw_kind`, which
   ICEs on a placeholder. The exceptions are the four reads that handle placeholders
   themselves: `Cx::kind`, `mentions_params`, the shape questions mapping through `ph_rep`,
   and `type_key_in`. A test fails on any other raw read.

### What is not shared in step 2

A definition keeps per-type instances, and the scan says why (a `VELT_LOWER_PROFILE` counter
reports it per reason), when:
- **A union in its footprint mentions a parameter**: `T | Baz`, or `V | X` with `X` a
  union. Tags of such a union follow `TyId` order (`canon_union` sorts members by id), and the
  union can collapse (`pick<Baz>` makes `T | Baz` just `Baz`). The later fix is a dictionary word
  per tag the body writes or tests, plus the collapse. That is also what package-cache.md wants
  ("union tags … move out of the code"). A type argument that *is* a union (sigx's `JSXChild`)
  is fine: the shared body only moves it, and its tags are read only by its own glue.
- **The body narrows, tests or branches on a value of a parameter's type:**
  - `Intrinsic::Truthy` of a `T`, which is per value for a union and constant for an object
    type;
  - `typeof x` with `x: T`;
  - `x instanceof C` with `x: T`;
  - the compile-time type predicates (`HoldsDynamic`, …).

  Step 4 makes these dictionary entries: `Truthy` and `Typeof` functions per type, like
  `AnyTruthy` but not through the `any` table, and the predicates as words. A counted box
  (an array or object type) has no class header, so `instanceof` of a `~rc` that may be one
  stays per type until then.
- Deferred type operators (`TyKind::Op`), generated per-type code (`generate/`), foreign
  boundaries, async functions, generators and closures that use a parameter's type. Each comes in
  a later step (order of work).

### Parameter attributes

`declare_locals` computes `ParamAttrs` per parameter from per-type facts:
- `noalias`, `readonly` and `frozen` from `unique_refs` and `holds_mutex`;
- `dereferenceable` from the class object's size;
- and `unfreeze_module_reach` changes them per type after the pass.

`unique_refs` and `holds_mutex` are shape questions, so they are in the key. The object size is
not. A parameter whose type is a placeholder (or a pointer to one) gets:
- `noalias`, `readonly` and `frozen` only where the key's answers allow them;
- `dereferenceable` only of the shape's own size: the value's size for `~agg`, 0 for a class
  object, whose size differs between members.

`unfreeze_module_reach` treats a shared instance's parameters as those of every type of its
dictionaries: it removes an attribute when any of them needs it removed. Debug builds don't
read the attributes for code generation. Release (section 3) counts what is lost under rule 1.

### Facts and the counted-types fixpoint

**Shared bodies are lowered in every pass, and their facts are replayed.** There is no pass that
lowers the program whole to share in: since #2221 and #2232 every pass is facts-only, and the
last one is the program, known only once `close_boxing` returns the `Boxing` it started with.
So a shared body has to leave the pass's facts exactly as the per-type bodies it stands for
would have. Otherwise the fixpoint ends in another `Boxing`, and code that doesn't share
changes too.

The replay works on the facts themselves, not on the methods that record them:
1. A shared body is lowered with the pass's facts, as any function is. Every decision it
   makes about a placeholder is a shape question (answered for the representative and checked
   at the end of the pass) or an ICE. So the facts it records are the per-type facts with each
   concrete type replaced by its placeholder.
2. After the body is lowered, every entry that names a placeholder is taken out of the pass's
   facts (`Facts::take_types`, boxing/replay.rs). The instance keeps those entries as its
   template.
3. For each dictionary, existing or made later in the pass, the template is added back with
   the placeholders replaced by that dictionary's types (`Facts::add_mapped`). Entries over
   concrete types, and the program-wide flags, stay as recorded.
4. `share_sites`, which the counted-`Atomic` diagnostic reads, gets the replayed shares at the
   site of the user code that asked for the dictionary.

Two parts of the facts are not plain sets of types:
- **Facts keyed by function** (`fn_tuple_writes`, `fn_read_params`) can't be replayed, since
  one shared `FuncId` stands for several instances. A shared body that records one with a
  placeholder type is an ICE in step 2. The scan keeps such bodies per type, which is the
  conservative subset: only bodies whose facts are plain sets of types share.
- **`callee_writes`** (boxing/tuples.rs) follows the calls of built functions and gives up at
  an indirect call. A shared instance's calls through its dictionaries are the functions they
  hold, so for a shared instance it follows those (`Cx::shape_callees`) and not the indirect
  calls.

Glue made for a shared std body (`ToAny`, `FromAny`) records what the std body would have
recorded at the site: it counts as std code for `note_array_view`, which ignores std's own
conversions.

The checks are the ones #2221 and #2232 used, run on every program in step 2's bars:
- VIR byte-identical between the facts-only passes and `VELT_LOWER_WHOLE=1`;
- the same pass count and the same `Boxing` growth per pass (`VELT_LOWER_PROFILE`) as with
  `VELT_SHAPES=off`.

The prototype passes both on the `Item[]` repro, `sigx_like`, `bench/compile/shapes/` and the
new golden.

**The full table, for the bodies of later steps.** What a template holds per `Facts` field:

| `Facts` fields | parameterized by | replayed per dictionary |
|---|---|---|
| `shares`, `identity`, `identity_borrows`, `tuple_writes`, `tuple_copies`, `transfers`, `captures`, `vtable_types`, `weak_seeds`, `dyn_types`, `object_types`, `array_view_targets`, `proxy_targets`, `lock_values` | a type template | the template instantiated. A predicate a fact is recorded under (`copied_object` in `note_dyn_identity`, `in_place_parts` for `identity_borrows`) is evaluated for the placeholder while the body is lowered, so it is a shape question: in the key, or an ICE. `close_share` evaluates its own predicates later, on the concrete types |
| `projections`, `upcasts`, `proxy_casts` | a pair of templates | both instantiated |
| `fn_tuple_writes`, `fn_read_params` | `FuncId` of the shared instance | not in step 2 (an ICE, the body stays per type). Later keyed by `(instance, dictionary)`, with `callee_writes` per dictionary |
| `fn_compared_keys`, `generic_any_keys`, `fn_edges`, `fresh_made` | `FnKey` over templates | instantiated keys |
| `dyn_to_any` | a `DefId` | as is |
| `any_identity`, `fn_compared`, `generic_any`, `any_set`, `any_has`, `any_delete`, `any_keys`, `any_reflect`, `any_number`, `any_call_keys`, `weak_object`, `object_from_dyn`, `proxy_new`, `read_indirect`, `unmet` | nothing (program-wide) | as is, once |

`share_sites` for the counted-`Atomic` diagnostic is replayed with the site of the user call
that requested the dictionary, so the report points at user code. A planned
`VELT_SHAPES=check` would lower each eligible instance both ways and compare the final
`Boxing` and the diagnostics, with this table as its coverage; step 2 checks the same by
comparing whole runs with `VELT_SHAPES=off`.

### How lowering represents a shape

The instance key stays `Work::Fn(def, targs)`. For a shared instance, each shared argument in
`targs` is a *placeholder*: a `TyKind::Param` with an index at or above `SHAPE_BASE`, interned
by lowering. HIR never contains one, and the HIR contract doesn't change. `FnLower::sub`, the
one place where instance types enter lowering, substitutes placeholders as it substitutes
concrete arguments, and `subst` leaves a placeholder as it is.

### Debug information

`describe_locals` gives every local a debug type. A local of a placeholder type (`const x: T`)
is described as what the shared code knows about it:
- for `~agg`, an array of `s` bytes named after the shape (`~agg(64, 8)`);
- for a pointer shape, a pointer to opaque data;
- for scalars and `~str`, their own type.

The frame's `$types` variable names the concrete types. Showing such a local per type, as
`Item` with its fields, is out of scope. Where it matters, sharing is limited instead:
- builds made for debugging (`debug_vars` on, as F5 builds) share only std code. User generics
  keep per-type instances, and their locals look as they do today;
- the std helpers' `T` locals show as bytes, and stepping into std is rare;
- other debug builds (`velt build`, `velt run`) share everything.

## 2. The dictionary ABI

There are two kinds of read-only statics, both `StaticData` with `mutable: None` and relocations,
exactly like vtables. So no VIR type, verifier rule or backend changes.

### Type tables

A **type table** (`_Gtype` *T*) is one per concrete type that reaches shared code as a type
argument. It uses the vtable layout (`lower/glue/vtable.rs`):
- the nine hidden slots, with the same constants: class id, a `struct` value's content
  equality and hash (interface tables only), transfer, name, share, format, clone, drop;
- then the dictionary slots:

| slot | entry | signature (as the glue's) |
|---|---|---|
| -7 | class id (a word) | |
| -6 | `Transfer` | `(p)` |
| -5 | the type's name, a static string (debuggers, section 4) | |
| -4 | `Share` | `(src, dst)` |
| -3 | `Format` | `(buf, p, depth)` |
| -2 | `Clone` | `(src, dst)` |
| -1 | `Drop` (`~rc`: `ObjDrop` / `QueuedDrop`, called when the count reaches zero) | `(p)` |
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
| 11 | `Truthy` (step 4): the truthiness of a value, per member for a union | `(p) -> bool` |
| 12 | `Typeof` (step 4): the `typeof` text of a value | `(p) -> ptr` |

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
- the words of the compile-time type predicates (`HoldsDynamic`, …), from step 4;
- the **function value** of each generic function a shared body takes as a value at a type
  built from a parameter (`xs.map(id<T>)`): the thunk of `id<concrete>`, or of the
  shared `id` with its constant dictionary, the same thunk that function values use anywhere
  (section 2, "The hidden parameter").

**Calls of other shared instances.** The callee's instance follows from the callee's own key,
which its footprint decides (section 1). Two members of the caller's shape can therefore need
two callee instances. A call whose callee instance is the same for every dictionary made so far
is a direct call that passes a sub-dictionary entry. Otherwise the entry is a pair, the
callee's code and its dictionary, and the call goes through the pair. Debug costs an indirect
call. Release folds constant pairs like any entry.

**Closures in shared bodies.** A closure literal in a shared body that mentions a parameter's
type, generic or not, captures the dictionary in its environment: its code is shared with the
instance's, and its own entries are entries of the instance's dictionary. That is step 4. In
step 2 the scan rejects such bodies.

**Polymorphic recursion**. `f<T>` calling `f<T[]>` is bounded today by sema's
`MAX_ARG_DEPTH` (64), and stays so. With shapes, `f<A[]>`, `f<A[][]>`, … can be one `~arr`
instance with a chain of dictionaries, each made at compile time when lowering meets it.
Dictionaries are statics and are never built at run time, so a chain is as long as the
instantiations lowering meets, which the same bound limits.

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
- **Function values.** A shared instance used as a function value, reached from a vtable slot,
  or passed to native code as a `NativeCallback`, goes through a thunk that passes the
  constant dictionary. Native code never sees a dictionary parameter. There is one thunk per
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
2. **Interface calls never take a dictionary**. A call through a vtable slot, generic or
   not, passes exactly the arguments it passes today. A dictionary appears only behind a thunk:
   a slot table entry of a shared method is the method's thunk with its constant dictionary,
   as for any function value. Interface dispatch costs what it costs today, plus the thunk's
   tail call where the method is shared, which release removes (section 3).
3. **Generic interface calls from shared code.** `x.wrap<T>()` on an interface value, inside an
   instance where `T` is shared, needs the index of `[T]` in the slot's tables. That index is a
   constant per concrete `T`, so it is a dictionary word. The call loads the instantiation
   table from the vtable (as today) and the index from the dictionary.

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
- **Instances reached by one dictionary are decided in lowering**. After the last pass, once
  `close_boxing` returns the `Boxing` it started with, lowering knows every dictionary of every
  shared instance. In release, an instance
  reached by exactly one dictionary gets its dictionary folded in place: the parameter becomes
  the constant, its calls drop the argument, and `vtable_loads` folds the loads. That is
  today's per-type code with no clone made. A program with one `Map` type pays nothing.
- **Hot call sites of an instance with several dictionaries** get a clone with the constant
  dictionary folded in, as `const_fields/specialize.rs` clones a callee for a known closure.
  This uses a budget of its own, not `MAX_CLONES`/`MAX_CLONE_COST`, which closure
  specialization (`sort(cmp)`) keeps for itself, so programs that share nothing don't change.
  Choosing call sites in loops is new work: `specialize.rs` doesn't look at loops today.

Step 3 adds `velt_opt` unit tests for the two-level fold (dictionary, then type table, then
function) and for the single-dictionary fold, with the release workstream that owns
`velt_opt`'s inliner. `addrs.rs` treats parameters as unknown, so the folds apply once the
dictionary is a constant, which is when they are needed.

### The per-package cache and facts traces

A shared instance refers to no per-type static: every type-specific thing it touches arrives
through its dictionary parameter. Its facts trace (package-cache.md) is its key plus the
program-wide flags shared code reads:
- `fn_identity`, `any_identity`, `any_writes` and the other `any` slot flags, which decide
  which `any` table slots exist;
- `weak_object`;
- whether the program has a lock set (`has_lock_set`) or proxies;
- whether `array_views` is empty.

Those flags are the same in most programs, so shared std instances are the entries that repeat
between programs. They are what std can ship compiled for the common keys (`~rc`, `~f64`,
`~str`, `~i32`, `~bool`).

A cache hit skips lowering, so the instance's outgoing fact templates (section 1, "Facts and
the counted-types fixpoint") have to be stored in the entry: `Served` gets `facts` next to
`calls`. Otherwise a cached shared instance would contribute nothing to the fixpoint. This
applies to package-cache.md's per-function entries too, and shared instances are where it
first matters.

Dictionaries and type tables are per program. They are the "statics with symbols" of
package-cache.md step 2: the cached caller of a shared instance refers to its dictionary by
symbol, and the program defines it. `PackageCode::lookup(func, args, facts)` is asked with the
placeholder arguments, and the cache keys them by their keys, as symbols do, never by `TyId`.

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
   as **call** or **inline**. A *call* is a callee LLVM would not inline in the per-type
   instance either: over the inline cap, measured as #2224 and its follow-up measure it
   (counting only the callees LLVM would actually inline), or cold. Anything else is *inline*:
   - a small `Hash`, `Eq` or `Share` glue that is a direct VIR call today but which LLVM
     inlines;
   - a type predicate turned into a branch;
   - the descriptor compared on the fast path of `as T`;
   - the share or drop of a small `~agg`.

   An indirect call also hides the callee's memory effects: after it, LLVM has to reload what
   it had in registers, such as `this`'s fields in `Map`'s probe loop. So an instance is shared
   in release only when all its reads are calls. An instance with inline reads goes to rule 2.
   The parameter attributes a placeholder loses (section 1) count as inline reads.
2. **Hot sites get a per-type copy back** (section 2): an instance reached by one dictionary is
   folded in place by lowering, and hot call sites of the others get specialized clones from
   their own budget.
3. **The gate decides, per family.** A family (the array-view helpers, `Map` methods, `Set`,
   `Record`, user generics) is shared in release only once:
   - every benchmark of `bench/nightly.sh` and the sigx scenarios is within ±3% by cachegrind
     instructions (`bench/compiler_ab.sh BASE HEAD`);
   - the hot ones (`bench/array_view`, `bench/btree`, `bench/concurrent_map`, `bench/json`) are
     also interleaved best-of-N by wall time.

   The gate also records each family's build CPU, IR lines and peak memory against base. A
   family within ±3% that saves nothing stays per type. Until it passes, the family stays per
   type in release, by a per-family switch in lowering (`shape::RELEASE_FAMILIES`). A family
   that fails is fixed (a concrete argument kept, a predicate left inline) or stays per type.

So what decides is:
- rule 1, computed by lowering;
- rule 2's single-dictionary fold, also in lowering, and its hot-site clones, in `velt_opt`;
- the measured gate, kept as a list in the code that only grows. A switch in
`VELT_SHAPES` picks the behavior for A/Bs without rebuilding the compiler: `off` and `all`
exist in step 2, `on` (the default made explicit) and `check` are planned.

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
| `instanceof` | `x instanceof C` with `x` of a concrete type: class ids are in object headers and class ranges are per class definition, so neither depends on which code tests them. With `x: T`, the body is not shared in step 2 (section 1), since a `~rc` may be a counted box with no class header |
| `typeof`, `Array.isArray`, truthiness of a `T` | not shared in step 2 (section 1). From step 4, the type table's `Truthy` and `Typeof` functions, whose answer depends on the value for a union |
| `===`, `Object.is`, `Map`/`Set` keys | inline where the class makes it bitwise or identity, otherwise `Same`, `SameValue`, `KeySame` and `Hash` of the type table. Hash values are the same, so iteration order is too |
| drop order, `dispose`, weak references | the same drop and trace glue, called at the same points of the same body |
| function identity | one thunk per `(def, targs)`, so as today (section 2) |
| threads (`transfer`, `shared`) | `Transfer` and `ManyCheck` per type |
| `e.stack` | Velt keeps no call frames, only the first line (`tests/golden/lang/error_stack.velt`), so this is unchanged |
| stack traces in debuggers, profilers and crash dumps | see below |

**Stack traces.** A shared instance's symbol names its shape: `arrayViewSync_T` followed by a
shape segment, its layout and a hash of the rest of its key, demangled
`arrayViewSync<~ptr.556ac4f9>`. Like other symbols it contains no `TyId`, so
`velt dev` matches it across versions. Debuggers and profilers show that name, which is correct
code-wise but doesn't say which type was meant. Debug builds name the types too:
- each dictionary starts with the names of its type arguments, a string object (and from step
  4 each type table holds its type's name, slot -5);
- the dictionary parameter is described to the debugger as `$types`, an object whose `types`
  field is that string (`debug-variables.md`).

A frame then shows `arrayViewSyncAt<~ptr.556ac4f9>`, and `$types` expands to
`types = "{ a: f64; b: string }"`. A value of the shared type shows its shape (`(~ptr) v`).
`$types` can't be typed as a name in LLDB's `frame variable`, so it shows with the frame's
other variables (`frame variable -P 1`).
`crates/veltc/tests/debugger.rs` checks this, next to the existing debug-variable cases.
Release strips symbols at link time, as it does today. Panic messages carry source locations,
not function names, and are unaffected.

**How it is tested:**
- **The bars.** Every golden passes, with output byte-identical between debug, where sharing is
  on, and release, where it is off.
- **`VELT_SHAPES=all`.** Shares every instance the scan accepts, including the families release
  doesn't share yet, and runs the whole golden suite in debug and release. This is proposed as
  one more CI job, so each leak or scan bug shows up as an ICE or a wrong output, not in a
  user's program.
- **`VELT_SHAPES=check`** (planned). Compares the counted-types facts and the diagnostics
  both ways (section 1, "Facts and the counted-types fixpoint"). Step 2 compares whole runs
  with `VELT_SHAPES=off` instead.
- **The enforcement tests** (section 1, "How the questions are enforced"), each checked to fail
  when broken on purpose:
  - code outside `boxing/` reading a counted-types set of types without `ph_rep` fails a test;
  - a raw kind read outside the four allowed fails a test;
  - the fixpoint checks run as a test (`lower_passes.rs`): the same growth and passes as
    `VELT_SHAPES=off`, and VIR identical to `VELT_LOWER_WHOLE=1`. Taking out the replay makes
    it fail.
- **New goldens, run with Node where they are valid TypeScript,** for what crosses a shared
  instance. In step 2:
  - views of two object types of one size with different fields, of classes and nullable
    classes, of a union and of strings (`array_view_shapes`);
  - nested views, a class that is a field of another view's element, and a weak-capable class
    beside a plain one (`array_view_shapes_nested`);
  - a type made `shared`, a class held by a `Mutex`, an `RwLock` whose values are in the lock
    set, a value passed to `spawn`, and an object type holding an `Atomic`, each beside views
    (`array_view_shapes_locks`, Velt-only);
  - a wrong conversion's `TypeError` text for class, object, union and nullable element types,
    each next to another view of its shape (`array_view_shape_error_*`; Node erases `as`).

  With the families of step 4: `Map` and `Set` keys of several class types, a shared method
  reached through an interface slot, a generic method called through an interface from shared
  code, function values of instances compared, and a shared instance passed to native code as
  a `NativeCallback`.
- **Key rows** (`shape_keys.rs`): a row left out of the key (`VELT_SHAPES_TEST_DROP`, debug
  compilers only), for a program whose view element types differ in that row alone, must make
  the end-of-pass check fire. The weak row is tested this way, with a weak-capable class beside
  a plain one, and so is the footprint: leaving `T[]` out (`array`) on two object types whose
  arrays differ in a counted-types set makes the check fire. A fact recorded per function in a
  shared body (`per-function`) is an ICE, also tested. For view element types the other rows never differ alone:
  - view element types are always counted, so the counted row can't differ;
  - a nullable class also differs from its class in share kind and class-ness, so the niche row
    can't differ alone;
  - aggregates of one size and different alignment also differ in their set memberships, so
    the alignment row can't differ alone.

  Their own tests come with the families of step 4, where they can.
- **A debugger case** (`debugger.rs`): a frame of a shared helper shows `$types` naming the
  concrete type, and the shape for a value of it.
- **A `velt dev` case** (`tests/reload/view_shapes`): a reload that adds an element type and
  edits the handler, hot-swapped (JIT) and restarted (`--exe`).

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

### Baseline, before any code

Measured with release-built compilers in a 4-core Linux container, so these are line counts, not
times, and no load affects them:
- main `a25dbab`;
- #2268's branch `gen-size` `5e34170`, which moves the `as T` slow path into `AnyUnbox` glue.

The programs:
- **repro:** #2271's ten-line program converting an `any` to `Item[]`;
- **sigx-like:** `bench/array_view`'s `sigx_like`, which converts `kids as JSXChild[]`, where
  `JSXChild` is a union;
- **scale 10k:** `bench/compile/scale/gen.py 10000`.

The helpers are the `arrayView*` functions of `std/lang/hooks/array_view.velt`. The counts are
non-empty lines inside their function bodies in `--emit vir` (debug) and `--emit llvm`
(release), summed per element type.

| program | element type | debug VIR, main | debug VIR, #2268 | release IR, main | release IR, #2268 |
|---|---|---:|---:|---:|---:|
| repro | `Item` (`~agg(64, 8)`) | 46,618 | 5,404 | 119,415 | 12,388 |
| repro | `string` (`~str`) | 6,063 | 5,581 | 13,672 | 13,118 |
| sigx-like | `JSXChild` (a union, `~agg`) | 18,355 | 8,296 | 41,406 | 40,058 |
| sigx-like | `string` | 6,068 | 5,586 | 13,672 | 12,932 |

What this says for the prototype:
- **#2268 makes an element type cost about the same, whatever the type:** about 5.5k debug
  VIR lines and 12-13k release IR lines for the family. That is the helpers' own code, compiled
  again per type. Sharing removes exactly that, so the saving is (element types − shapes) ×
  that cost.
- **These two programs have one element type per shape** (`Item` and `string`; `JSXChild` and
  `string`), so sharing saves nothing on them. They are the "nothing changes" cases. The win is
  in programs with many view element types, which is what small-page's 3.32M lines of helpers
  mean: at main's sizes, hundreds of instances. The prototype's counters report small-page's
  element types and shapes first, before any timing, and that ratio predicts the rest.
- **A union target still writes much of its conversion inline in release** (`JSXChild`: 40k
  lines on #2268 against 41k on main; #2268 outlines the slow path of object types). Shared
  code calls slot 6 for every target, so in shared instances unions get the out-of-line path
  as well. Where the per-type instance would have inlined the union's member comparisons, that
  is rule 1's *inline* read for release.

**Scale 10k, debug:** the build takes 1.05 s (lower 138 ms, codegen 528 ms; `-v`). The generic
instances are user code, 121 instances each of `common.count`, `first` and four `Box<T>`
methods, one per generated class. They come to 17,182 of 194,664 VIR body lines (8.8%), and
sharing makes them 6 instances, or 12 if the classes split between `~rc` and `~own`. In release
all of them are inlined into their callers, and none is left in the IR, so the release scale
numbers should not move. That makes them a check that release is unchanged, not a target.

New counters in `VELT_LOWER_PROFILE`:
- shared instances;
- per-type instances they replace;
- dictionaries and type tables, with their words;
- per family: instances before and after;
- the instances the scan rejected, by reason, so the next family's work starts from data.

Expected effect, to check against these numbers: the helpers' instances drop to one per key
used, against one per element type today. Small-page's element types are mostly object types
and unions, so its keys are mostly the sizes among them, split further by the question table
(weak, lock set, niche), plus `~str` and the pointer keys. Its 3.32M lines of helpers go toward
that many families of about 13k release IR lines each, plus the dictionaries (a few words per
type). Whether that is a large cut depends on how many element types share a key, which
nothing in this repository shows (below).

### Step 2's bars

The baseline shows that the in-repository programs have one element type per key (the repro,
`sigx_like`), so they can't measure a saving. Scale 10k/100k has no views, so for step 2 it is
only a check that the scan and the footprint don't cost anything. Small-page isn't in this
repository. So step 2 starts with what makes the result reproducible:
1. **The counter first, before the placeholder machinery.** `VELT_LOWER_PROFILE` reports, per
   shared family, its element types and the distinct keys they fall into, and the instances
   the scan rejects, by reason. Its first report is small-page's, posted on #2271. Open
   question 4 (aggregate sizes) and the rows of the question table that split keys are
   settled by that number. If small-page's element types fall into nearly as many keys as
   types, step 2 stops there.
2. **A generated benchmark in the repository,** `bench/compile/shapes/` (generator and
   `run.sh`, like `scale/`). It converts `any` arrays to N view element types over a few
   keys, by default 40 object types of three sizes, a union and `string`. Its expected cut is
   computed from the baseline: about 5.5k debug VIR lines per element type of the helpers, so
   about (43 − 5) × 5.5k = 0.2M debug VIR lines. Head has to reach at least 80% of that.
3. **The bars as numbers:**
   - goldens pass, with debug and release output byte-identical;
   - on `bench/compile/shapes/`, the helpers' debug VIR lines down by at least 80% of the
     expected cut, and debug build CPU down;
   - on hello and scale 10k/100k, debug build CPU and peak RSS within noise (instructions
     within ±1%);
   - release VIR byte-identical to base for every golden and benchmark program;
   - small-page's helpers' debug VIR, with the cut reported against the counter's prediction.

**Without #2268.** Step 2 doesn't depend on it. The prototype's `FromAny` glue wraps what `any_to`
writes at a site today (`any_unbox`), so it works on main. When #2268 lands, `FromAny`'s body
becomes #2268's fast path plus its `AnyUnbox`, and nothing in the shared code changes.

## Order of work

1. **This note.**
2. **Prototype, debug only, the array-view sync helpers.** It covers `arrayViewSync`,
   `arrayViewSyncAt`, `arrayViewSyncLen`, `arrayViewFlush`, `arrayViewFlushFrom`,
   `arrayViewFlushAt` and `arrayViewLen` (not `arrayViewSort`, whose closure and comparator
   wait for closures). In order:
   1. the counter, and small-page's first report (section 5, "Step 2's bars");
   2. `bench/compile/shapes/`;
   3. the question table enforced: `Boxing`'s sets behind accessors, `Cx::kind` ICEing, and
      the raw `types.kind` reads listed;
   4. the placeholders, the footprint scan and the end-of-pass check;
   5. the dictionaries on the generalized growing tables, and the hidden parameter;
   6. `ToAny` and `FromAny` glue;
   7. shared bodies in every pass with their facts replayed (section 1), and `VELT_SHAPES`.

   It is checked against the bars in section 5.
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
| the passes and the counted-types fixpoint (`lower/mod.rs`'s pass loop, `lower/boxing/`, `view_unswitch*`, `spill/`) | lowering | `Boxing`'s sets behind accessors (section 1); taking a shared body's facts out and replaying them (`boxing/replay.rs`); `callee_writes` through dictionaries (`boxing/tuples.rs`); in the pass loop, the shape state set up per pass and its end-of-pass check, two calls and no new pass. Agreed on #2189 before it lands |
| `any` operations (`any_*.rs` sites, `any_unbox`) | `any` operations | the fast path of `as T` reading the descriptor from slot 5 and the slow path calling slot 6 when the target is a placeholder: one branch at the top of `any_unbox` |
| glue and vtables (`lower/glue/`, `lower/any.rs` tables) | release backend and glue | type tables built from the existing glue builders; `generic_slots.rs` generalized into the growing table both use |

Step 2's own code is a new `lower/shape/` module with the key, the scan, the type tables and
dictionaries, the glue and the rewrite of pseudo ids into dictionary loads. It also touches:
- the call sites of the helpers (`array_view.rs`);
- the hidden parameter in `abi.rs`;
- the shape queries' placeholder case (`types.rs`, `boxing/`);
- one new `LowerOptions` field, `shapes`, which the driver sets for debug builds. `ast.rs`, HIR,
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
4. **Aggregates of many sizes.** `~agg` is keyed by size, so a program whose view element types
   are object types of twenty sizes gets twenty instances. If small-page's counters show that,
   a size class merges them: values kept in slots of the class's size (16, 32, 64, 128 bytes …)
   and copied by the dictionary's exact size, with one instance per class. Locals and array
   strides then differ from the per-type layout, so arrays of `T` inside shared code would still
   use the exact stride, read from the dictionary. That is more change than step 2 needs. It is
   decided by the counters, not up front.
