# Generic instances shared by shape

Status: partly implemented. Issue: [#2271](https://github.com/velt-lang/velt/issues/2271).

## Summary

Velt compiles a generic function once per list of type arguments. With shape sharing, lowering
compiles one instance per *shape* of the type arguments (what the code may depend on: layout,
ownership, a closed list of per-type facts) and passes a *dictionary* of the type-specific code as
a hidden parameter, as Go does with GC shapes and dictionaries. Debug builds share the
array-view sync helpers of the prelude this way; the observable behavior of a program is the same
as with per-type instances.

## Motivation

Much of a large program's generated code is standard library generic instances: `Map` and `Set`
methods, the array-view sync helpers, `Record`. Most of that code does not depend on the type
argument, and the rest depends on a few of the type's operations. Converting an `any` to an array
of an object type, for example, instantiates the array-view helpers per element type, and their
bodies differ between element types only in the calls they make. Rust compiles one copy per type,
which is a large part of its build cost; Go compiles one copy per shape.

Two things differ from Go:

- **Ownership is part of the representation.** A class object is either counted (a count word in
  front of it) or owned by one reference, depending on what the whole program does
  (`crates/velt_vir/src/lower/boxing/`). The two need different inline code for the same 8-byte
  pointer.
- **Shapes exist only during lowering.** Sema and HIR are unchanged; sharing is a lowering
  decision.

## Design

### The shape key

A shape is everything about a type argument that a shared instance's code may depend on without
asking a dictionary. Two lists of type arguments get the same code only when every such answer is
the same for both; every other difference goes through the dictionary.

Lowering compiles code inline from per-type facts in many places, so the key is not a table of
type classes but the answers to a closed list of **shape questions**
(`crates/velt_vir/src/lower/shape/key.rs`):

| Question | Why inline code depends on it |
|---|---|
| layout (`ty`: an aggregate as its size and alignment), storage float | moves, locals, strides; arithmetic conversions |
| share kind | the copy written at a share |
| needs drop | whether a drop is emitted |
| null niche | `T \| null` as a pointer |
| class object | parameter shape |
| boxed, counted | count increments and decrements inline |
| proxiable | typed-proxy tests at reads and drops |
| interior (`unique_refs`) | `noalias` (vir.rs invariant 9) |
| lock set (`lock_counted`, `reaches_lock_set`, `shared_reads`) | lock-set counting, read-mode pinning at stores and calls |
| weak-capable | the three-way release |
| union | truthiness and narrowing of the value |
| copied across, transfers | what a hand-over to another task writes |
| mutex (`holds_mutex`) | `frozen` |
| atomic (`holds_atomic`), shares as counted (`shares_as_counted`) | whether, and how, an operand is copied before a later one is evaluated |
| array view | the view sync at every access |

and membership in each of the counted-types sets of `Boxing`. A new inline dependency is one more
row, not a new class. So `Map<string, A>` and `Map<string, B>` share an instance when A and B
answer every row alike; a weak-capable A against a plain B, or an A in the lock set, splits them.

**The footprint.** The questions are asked of every type in a shared definition's body that
mentions a type parameter, not only of the argument: for the array-view helpers, `T` and `T[]`.
The key is the answers for each type of the footprint.

**Placeholders.** A shared instance is lowered with a *placeholder* for each shared type
argument: a `TyKind::Param` with an index at or above `SHAPE_BASE`, interned by lowering. HIR
never contains one. Two calls share a placeholder only when their keys are equal, so every type
built from a placeholder has the same answers for every member of the shape, and memo tables keyed
by `TyId` cannot mix two footprints. The instance key stays `Work::Fn(def, targs)`, with
placeholders in `targs`.

**The end-of-pass check.** At the end of each pass, every type built from a placeholder that
shared code asked about is instantiated with every member of the shape, and its answers are
compared with the representative's. A difference is an ICE: the footprint missed a type. A key too
coarse is therefore caught, never compiled into wrong code.

### How the questions are enforced

1. **The shape questions answer for the representative.** A placeholder's representative is the
   first concrete type of its shape. Each shape question maps a placeholder, or a type built from
   one, to the same type built from the representative first (`Cx::ph_rep`). The key makes every
   member give the representative's answers, so that answer is right for all of them.
2. **`Boxing`'s sets are read through the representative** by the same mapping. Nothing is put
   into or taken out of `Boxing` for a placeholder.
3. **Every other question about a placeholder is an ICE.** `Cx::kind` and `Cx::raw_kind`, through
   which code that matches on a type's kind reads the type table, ICE on a placeholder.

### Facts and the counted-types fixpoint

Lowering runs passes that only gather facts until the counted-types analysis reaches a fixpoint,
and the last pass is the program. A shared body must leave the pass's facts exactly as the
per-type bodies it stands for would; otherwise the fixpoint ends elsewhere and code that does not
share changes too. So shared bodies are lowered in every pass, and their facts are replayed
(`crates/velt_vir/src/lower/boxing/replay.rs`):

1. A shared body is lowered with the pass's facts. Every decision it makes about a placeholder is
   a shape question or an ICE, so the facts it records are the per-type facts with each concrete
   type replaced by its placeholder.
2. After the body is lowered, every entry that names a placeholder is taken out of the pass's
   facts (`Facts::take_types`) and kept as the instance's template.
3. For each dictionary, the template is added back with the placeholders replaced by that
   dictionary's types (`Facts::add_mapped`). Entries over concrete types and program-wide flags
   stay as recorded.

Facts keyed by function (`fn_tuple_writes`, `fn_read_params`) cannot be replayed, since one shared
`FuncId` stands for several instances: a shared body recording one over a placeholder is an ICE.
`callee_writes` follows a shared instance's calls through its dictionaries
(`Cx::shape_callees`) rather than giving up at the indirect calls. Glue made for a shared std body
(`ToAny`, `FromAny`) records what the std body would have recorded at the site.

A comparison through glue records which function values the compared type holds
(`fn_compared_keys`), which a placeholder cannot say: the body records the type
(`Facts::glue_compared`), and `Cx::replay` records the keys for each dictionary's type.

### Dictionaries

A dictionary is a read-only static, one per shared instance and list of concrete type arguments
(`crates/velt_vir/src/lower/shape/dict.rs`). It starts with the names of its types as a string
object (for debuggers) and continues with entries:

- the address of each function the shared body needs at a placeholder type: glue (drop, share,
  clone, format, equality, hash), the conversions to and from `any` (`shape/glue.rs`), and a
  generic callee that stays per type;
- for a call of another shared instance, that instance's dictionary for the substituted
  arguments.

While a shared body is lowered, a function it needs at a placeholder type gets a pseudo id instead
of being built; afterwards the pseudo ids become loads of dictionary entries and indirect calls
(`shape/rewrite.rs`). Entries are found while the body is lowered, so dictionaries grow as
`glue/generic_slots.rs`'s tables do: a new entry is appended to every dictionary made so far, and
one made later starts with every entry known by then. Dictionaries are statics with relocations,
like vtables, so no VIR type, verifier rule or backend changes.

**The hidden parameter.** The dictionary is a hidden pointer parameter of the shared instance,
after the user's parameters and before the caller's location of a caller-tracking instance and the
out-pointer of an aggregate return. So the hidden parameters keep their order, a closure
environment stays first, and the masks and attributes that index user parameters do not shift.
Code of concrete types passes the dictionary as a constant static; shared code passes a
dictionary loaded from its own.

**Symbols.** A shared instance's symbol names its shape: its layout and a hash of the rest of its
key (`~agg64x8.556ac4f9`, `~str.…`). Like other symbols it contains no `TyId`, so `velt dev`
matches it across versions; dictionaries are data rebuilt per version.

### Debug information

A local of a placeholder type is described as what the shared code knows about it: an aggregate
as bytes named after its layout (`~agg(64, 8)`), scalars as their own type. The dictionary
parameter is described to the debugger as `$types`, an object whose `types` field is the
dictionary's string of type names, so a frame of a shared helper says which types it runs for
(see [variables in the debugger](debug-variables.md)). LLDB's `frame variable` cannot name
`$types`, so it shows with the frame's other variables (`frame variable -P 1`).

### Semantics

Shared code never answers a per-type question itself. Everything whose answer depends on more than
the shape is a dictionary entry built per type by the same glue builders as a per-type instance
uses, or a placeholder query that ICEs. So a value's observable behavior is produced by the same
code, in the same order, as in a per-type instance:

| Observable | In a shared instance |
|---|---|
| `console.log`, `String()`, templates | the type's format glue, so class names, quoting and nesting are the type's |
| `TypeError` from `x as T` | the type's `FromAny` glue, with the type's name |
| other errors and panics | source locations of the body, the same in every instance; caller-tracking passes its location on |
| `===`, `Object.is`, `Map`/`Set` keys | the type's equality and hash glue; hash values, and so iteration order, are the same. The equality glue of a type that is otherwise compared in line (a number, a string, an object by identity) is that comparison (`same_body`) |
| drop order, `dispose`, weak references | the same drop glue, called at the same points |
| `e.stack` | unchanged: Velt keeps no call frames |

Panic messages carry source locations, not function names, and are unaffected.

## Compilation

`crates/velt_vir/src/lower/shape/` holds the key (`key.rs`), the dictionaries (`dict.rs`), the
`any` glue (`glue.rs`) and the rewrite of pseudo ids into dictionary loads (`rewrite.rs`). The
shared definitions are listed in `shape/mod.rs` (`SHARED`): the prelude's array-view sync helpers
(`arrayViewSync`, `arrayViewSyncAt`, `arrayViewSyncLen`, `arrayViewFlush`, `arrayViewFlushFrom`,
`arrayViewFlushAt`, `arrayViewLen`) and the array `truncate` they call at their element type.

`LowerOptions::shapes` turns sharing on; the driver sets it for debug builds.
`VELT_SHAPES=off` turns it off and `VELT_SHAPES=all` turns it on in every build, for A/B
comparisons and tests. Release builds share nothing by default, and their VIR is unchanged.

Tests:

- `crates/veltc/tests/lower_passes.rs` checks the fixpoint: the same passes and counted-types
  growth with sharing as without, and VIR identical to a whole-program lowering
  (`VELT_LOWER_WHOLE=1`).
- `crates/veltc/tests/shape_keys.rs` leaves a row out of the key (`VELT_SHAPES_TEST_DROP`, debug
  compilers only) for a program whose element types differ in that row alone, and requires the
  end-of-pass check to fire: the weak row, with a weak-capable class beside a plain one, and the
  `T[]` footprint (`array`), with two object types whose arrays differ in a counted-types set. A
  fact recorded per function in a shared body (`per-function`) must be an ICE. The hook can also
  drop the `counted`, `niche` and `align` rows, which view element types never differ in alone.
- End-to-end tests of views over types that cross a shared instance: object types of one size
  with different fields, classes and nullable classes, unions, strings, nested views, weak-capable
  classes beside plain ones, values in the lock set, and the `TypeError` text of a wrong
  conversion for each kind of element type (`tests/golden/*/array_view_shapes*`,
  `array_view_shape_error_*`, `array_view_loop_shapes`).
- `crates/veltc/tests/debugger.rs` checks that a frame of a shared helper shows `$types`, and
  `tests/reload/view_shapes` reloads a program that adds an element type, hot-swapped and
  restarted.
- `bench/compile/shapes/` generates a program converting `any` arrays to N element types over a
  few shapes and reports VIR lines, shared instances, build CPU and peak memory with sharing on and
  off.

## Runtime cost

In a debug build a shared instance pays an indirect call and a dictionary load where a per-type
instance has a direct call. Debug builds run only velt_opt's cheap passes, and the glue is a call
in both versions, so the difference is small; debug run time is not gated, while debug build time
and memory are what sharing reduces. Release builds are unchanged.

## Alternatives considered

**A dictionary field in the receiver.** Swift and C# keep type information in the object. It would
cost every `Map` and `Set` object a header word, and it covers only methods: `arrayViewSync<T>` is
a free function, a method's own type parameters are not the receiver's, and a constructor makes its
receiver. A hidden parameter covers all of these with one rule.

**A table of type classes as the key.** Inline code depends on more per-type facts than any fixed
set of classes captures (the weak release, union tags, parameter attributes, lock-set reach,
nullable pointers). A list of answers to the queries lowering actually makes is exact, and the
end-of-pass check catches a missing row.

**One `~ptr` shape for counted and owned pointers.** Merging them would halve some instance counts
at the price of a dictionary call per share. They stay apart, so one key serves debug and release.

## Limitations

- Builds made for debugging share the same std helpers as other debug builds; their element-type
  locals show as bytes named after the shape, not as the concrete type with its fields.
- Interface calls never take a dictionary: interface dispatch is unchanged
  ([#375](https://github.com/velt-lang/velt/issues/375)'s per-instantiation slot tables).

## Not yet implemented

- **More families.** `Map`, `Set`, `Record` and their helpers, then every eligible std generic,
  then user generics, chosen by a scan of each definition's footprint rather than a fixed list. A
  definition stays per type, with the reason reported by `VELT_LOWER_PROFILE`, when a union in its
  footprint mentions a parameter (union tags follow type order, and `T | Baz` can collapse), when
  its body narrows, tests or branches on a value of a parameter's type (`typeof`, `instanceof`,
  truthiness, compile-time type predicates), or when it uses a deferred type operator, generated
  per-type code or a foreign boundary.
- **Type tables.** One read-only table per concrete type that reaches shared code, in the vtable
  layout: the hidden slots (class id, transfer, name, share, format, clone, drop) followed by
  equality, identity, key equality, hash, the `any` descriptor, the conversion from `any`, the
  `typeof` tag, tracing, the lock-set type word, and `Truthy` and `Typeof` functions that make
  narrowing on a parameter's type shareable. A slot is filled only when shared code reads it.
- **Closures, async functions and generators** in shared bodies, with the dictionary as an
  implicit capture; function values of shared instances through one thunk per `(def, targs)`
  passing the constant dictionary, so function identity is unchanged and native code never sees
  a dictionary; generic interface methods called from shared code, with the slot index as a
  dictionary word.
- **Release builds**, family by family, when run time stays within the performance rule: an
  instance shares only when its dictionary reads are calls LLVM would not inline anyway; an
  instance reached by one dictionary has the dictionary folded in place, and hot call sites of the
  others get clones with a constant dictionary, which velt_opt's `vtable_loads` and `constfold`
  turn back into direct calls.
- **Glue shared by shape:** the drop, clone and format of `Map<K, V>` objects and arrays of
  aggregates, with the element glue from the type table.
- **Cached shared instances.** A shared instance refers to no per-type static, so its code depends
  only on its key and a few program-wide flags, which makes shared std instances the entries that
  repeat between programs in a cache of compiled code.
