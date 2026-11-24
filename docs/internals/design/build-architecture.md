# Build architecture: units with interfaces

Status: partly implemented. Issue: [#2943](https://github.com/velt-lang/velt/issues/2943).

## Summary

A Velt program is built from **units**: the standard library, each package version it depends
on, and the app. Each unit is checked, lowered and compiled against the **interfaces** of the
units it imports, and never changed by the units that import it. That makes a unit's checked
form, IR and machine code cacheable on their own: std and packages are checked and compiled once
(std once per toolchain, a package once per version), and a build after an edit redoes only the
edited unit, a program-level specialization stage, and the functions whose inputs changed.

By default the whole program is one unit at every stage. This note describes the target
architecture, the rule that makes it sound, the pieces that exist (units in sema behind a flag, the frozen
boundary, stable type identity, the std identity oracle, the per-function caches), and the rest.

## Motivation

A build costs O(std + packages + app) whatever the edit. On `examples/todo_api.velt` after a
one-line edit in `main`, a debug build with a dev-profile compiler took about 1 s, and 69k of the
77k lines it loaded were std:

| stage | ms | why it is redone |
|---|---:|---|
| parse | 120 | every std file is parsed again |
| sema | 355 | std's checked meaning depends on the program; ownership passes are 45% of it |
| lower | 344 | lowering reads whole-program facts, so every function is lowered again |
| optimize | 41 | |
| codegen | 89 | nearly every function comes from the cache, which keys Cranelift IR: everything before it already ran |
| link | 67 | |

Sema cannot be cached as it stands because a function's checked meaning (ownership modes, error
set, vtable slots, representation) depends on code elsewhere in the program, std's on user code
included. An inventory of sema and lowering finds 83 such whole-program inferences and decisions.
Sema also has fixed points that discard the whole check and redo it, std included: grown error
sets of function types, grown union views, the sharing retry, and lazily checked std bodies
needed too late (the round loop in `crates/velt_sema/src/rounds.rs`).

Output caches cannot fix this: their input is the whole program, and an edit changes it. The more
of the runtime is written in Velt (the async executor, I/O), the more std source every build
checks, lowers and compiles.

## Design

### Units

A unit is std (one unit; finer splits are possible later), a vpm package version, or the app.

### The rule

A unit's **checked form** depends only on its sources, the **interfaces** of the units it
imports, and the options that change checking. Its **IR** depends on the same inputs. Its
**machine code** depends on its IR, the target and code options, and the program facts each
function read. Nothing a dependent does changes any of these.

The rule is enforced by structure, not by sampling: units are checked in dependency order, each
to completion, and a finished unit's definitions are frozen. The app's check reads std's
interface and cannot write to std's definitions. Every place that writes across the boundary
fails loudly and takes one of the modular forms in "Not yet implemented".

Whole-program facts live in exactly two places:

1. **Program tables and the glue unit**: class ids and ranges, vtables and interface tables,
   `any` dispatch, module-state layout, per-program hook bodies (`Program::glue_bodies` in
   `crates/velt_sema/src/hir/mod.rs`), class objects.
2. **The specialization stage**: representation choices made from the facts of the linked
   program, keyed per function.

### Stable identity

A unit's interface and cache keys must name types the same way in every program. A type's
canonical spelling (`crates/velt_sema/src/canon_key.rs`) is by structure and by qualified name,
never by `TyId`, `DefId`, alias name or load order. Unions order their members by it, so a union
std declares has the same variants in the same order in every program. A type that reaches
itself through a union (`type Json = … | Json[]`) is spelled `^` inside itself.

## Compilation

### Units in sema

`CheckOptions::units` (`crates/velt_sema/src/units.rs`) checks std as a unit of its own: std is
collected and every std body checked first, then std's definitions are frozen and the app is
collected and checked against them. `velt check`, `velt build`, `velt run` and `velt test` turn
it on with `VELT_UNITS` (`crates/veltc/src/driver/all_std.rs`); they then load every std file and
check nothing lazily. With `VELT_UNITS=1` the passes after the bodies (generated code,
ownership, throws) run once over every definition with the freeze lifted; with `VELT_UNITS=2`
std's unit is finished first and the app's stage runs them over the app with std's definitions
frozen.

A build by units lowers the program that check produced, so the program built is the one checked
by units, with three differences from a whole-program build:
- It lowers and starts all of std, not only what the program reaches: every std file is a root,
  so hello's release VIR grows from 8 functions to 228 and its `velt_main` runs the initializers
  of std modules it never imports. Its size, start-up and run-time instruction counts aren't
  comparable with a whole-program build's, and a program can fail by units for a std module's
  initializer rather than for sema. Lowering only what the program reaches is U5's program set.
- It links no prebuilt std objects: those are built from the whole-program check's std, so std
  is compiled with the program.
- A source generator's host (`sess.generator_build`) is still built as one program, as a check
  by units builds it.

The nightly's `golden-units` runs every end-to-end test this way; the goldens that fail only by
units are listed `units` in `crates/veltc/tests/golden_known_failing.txt` (#2943's U6 list).

The definition table (`crates/velt_sema/src/ctx/def_table.rs`) is indexed by `DefId` with a
frozen prefix: a finished unit's definitions. Every way to write checks the mark. A write below
it is a later unit changing a finished one; `VELT_UNITS_FREEZE=panic` stops with an ICE, and the
default counts each write by the caller's location and prints the counts at the end of the check,
so one run over the end-to-end tests lists every place that still writes across the boundary.

Some facts about std's definitions depend on the program, and a whole-program check writes
them during the passes after the bodies:
- an override or instance raises a std method's or a std generated function's pass modes
  (`ownership::mutation`'s groups);
- a program implementation raises a std interface slot's modes;
- a program member's errors join a std function's throw group (`throws::infer`).

The app's stage keeps these beside std's definitions instead (`units/raised.rs`). The app's
passes read them in place of the definitions' own (`modes_of`, the slot and throws readers),
and they are written onto the definitions once the freeze lifts to build the program, as a
whole-program check had written them by then. A saved unit holds none.

A std generated function whose instances always modify the same parameters, the generator's
output or input stream, says so with `@modifies` (`decorators/modifies.rs`; source-generators.md
"Std's generators"). Those parameters take their final mode where std declares the function, so
no program's instance raises them; that alone removes the raise nearly every program made (the
JSON writers' `w`). The list is a floor: an instance also runs the program's own code (a
`toJSON` that changes its object), which can still raise another parameter, kept beside the
declaration as above.

A program's override of a std method that has no vtable slot (`Map.get`) adds one to std's
class, which the app's calls through a std-typed receiver dispatch through
(`collect/program_slots.rs`, #3684). It is kept beside the class like the facts above
(`Ctx::vtable_of`, `vslot`) and written when the freeze lifts. Std's checked bodies keep their
slot numbers, so the slot goes after the longest vtable of the class's std subtree, and every
std class of the subtree gets it; the indices in between pad with an entry the class already
has, which the passes pairing a class's slots with its base's skip (`Ctx::vtable_entries`).

Other writes are still counted: #3543's other sites.

### The std identity oracle

`VELT_STD_SUMMARY=<file>` makes a check append a line per checked std definition: its name, the
hash of its interface (what callers see) and the hash of its whole checked form
(`crates/velt_sema/src/interface/std_defs.rs`, `crates/veltc/src/driver/std_summary.rs`). The
hashes canonicalize what differs between two checks of the same code: a `DefId` becomes the
definition's name, a `TyId` the hash of its type by structure, and source positions are left out.

`bench/compile/std_identity.sh` compares every end-to-end test's std definitions with those of
`velt check --std` and names each that differs. `VELT_STD_SUMMARY_SHOW=<part>` writes the hashed
form of matching definitions, to diff two checks. `VELT_STD_SUMMARY_EAGER=1` checks every std
file with the program, as a cached std unit would hold it, so differences that only lazy checking
makes drop out. Under the structural rule the oracle is a regression test of the frozen boundary,
not the method for finding dependencies.

### Caches that exist

`velt_build_cache` owns every compiled artifact on a machine (`<root>/v5/<kind>/`, content-keyed,
garbage-collected least-recently-used under a size limit, safe for concurrent writers). Units add
kinds and keys to it, never a second root. The stages it serves:

| Stage | Reused between builds | Kind and module |
|---|---|---|
| Parse of std files | per file | `std-ast` (`crates/velt_loader/src/std_ast.rs`) |
| std's module index | per std module | `pkg-interface` (`crates/velt_loader/src/std_index/`) |
| Generators | which instances, which code | `crates/veltc/src/generate/cache.rs` |
| std functions | per function, by its lowered code | `stdfn` (`crates/veltc/src/driver/std_cache/`, [std-object.md](std-object.md)) |
| LLVM codegen units (release) | per unit, by its IR text | `llvm` |
| Cranelift (debug) | per function | `cl` |
| Link | the link stamp, when nothing changed | `crates/veltc/src/driver/stamp.rs` |

`bench/compile/edit_loop.sh` measures the edit loop per stage (check, debug and release builds;
cold, warm and after a body edit) in wall-clock time and instructions.

## Runtime cost

A function compiled once for a unit is not inlined into callers in other units unless the unit
exports its body. The std cache shows the size of that cost: `bench/json/parse_typed.velt` built
with every std function served from the cache (`VELT_STD_OBJECT=1`) retires about 30% more
instructions than without it, because the reader's functions are linked instead of inlined into
the loop. Units therefore export bodies for callers (generic, `inline()` and small functions) and
release builds inline across units through them; a unit's cached code is used in release builds
only where run time stays within ±3% of a whole-program build.

## Alternatives considered

**Output caches only.** Caching each stage's output keyed by its input (the std cache, the
Cranelift and LLVM caches) reuses codegen but not the front end: the input of sema and lowering is
the whole program, so every edit misses. They remain as the code layer below units.

**Removing whole-program dependencies one at a time, as the oracle finds them in tests.** It is
slow, and a clean sample proves nothing about programs it did not run. The frozen boundary makes
every dependency fail where it happens.

**Conservative slot contracts.** Giving an overridable method's slot fixed, conservative modes
lets any override fit without adapters, but a slot's setter then cannot take ownership of its
argument and copies it. Measured in a prototype that gave `Error.name` a slot, a hot write through
the slot retired over six times the instructions of the direct field write even with
devirtualization, while reads and throw/catch were unchanged once devirtualized. So a slot's
contract is the declaring unit's own inferred one, and an override that needs another pays with
an adapter.

**Sealed classes or an `@open` marker for overridable std members.** Any class can be extended in
TypeScript; slots for every non-static method plus program-level devirtualization recover the
dispatch cost without a marker.

## Not yet implemented

### A unit's interface

Modelled on what Lean (`.olean` borrow flags), Koka (`.kki` borrow info), Go (escape tags in
export data), GHC (`.hi` demand signatures and unfoldings) and Swift (module interfaces, SIL for
`@inlinable`) store. Each item has its own hash, so a dependent records exactly what it read.

1. **Declarations and checked signatures**, with stable keys (`std/fs/files.velt::readFile`).
2. **A contract per function and per slot**, inferred bottom-up inside the unit:
   - parameter and receiver modes (Borrow, BorrowMut, Move);
   - the error set, in symbolic form: own errors plus the error variables of its function-typed
     parameters (Koka's effect polymorphism; std already writes `map<U, E>` by hand);
   - per parameter: retained, may cross threads, shares captures, may keep a borrowed function,
     may reach module state;
   - fresh return, `holds_promise`, async-ness.
3. **Slots of every overridable member.** Every non-static method of an exported class has a slot
   in the unit's IR, with the declaring unit's own contract. A slot reference is symbolic; the
   link numbers it.
4. **Bodies for callers**: generic functions, `inline()` and small functions (getters, leaf
   functions, worker/wrapper entries) and generator inputs, as versioned IR with an inline cost.
   Exported bodies are limited to what pays, because every exposed body turns a body edit into
   dependent rebuilds.

The interface is a versioned binary encoding, like `velt_syntax`'s `encode_module`.

### Three mechanisms

**A. Modular inference.** Ownership, error sets, sharing and thread facts are inferred per unit.
Fixed points stay inside a unit (or an import cycle within it). A dependent is checked against
the contracts and never changes them. Where it needs more, it adapts at its own boundary:

- an override or implementation whose modes differ from the slot's gets an **adapter thunk** in
  its own vtable entry (as Swift's vtable and witness thunks);
- a closure passed where a parameter's contract does not allow its errors is checked against the
  contract (or wrapped, when the contract says "may throw anything");
- diagnostics follow from contracts: "this closure modifies captured `i`; `enqueue`'s callback
  may run on another task" is stated by std's signature, not by a whole-program walk.

**B. The specialization stage.** Units are cached as checked, lowered IR with symbolic
references. At link time a program-level stage, a ThinLTO-style thin link over summaries:

- numbers classes, slots, statics and module state;
- devirtualizes a slot that every class of the receiver's family implements with the same
  method: Velt builds closed executables, so the link knows the full class set;
- makes the representation choices that need program facts (counted types, views, identity
  flags, lock modes);
- keys each function's machine code by its IR hash and the facts it read. Only functions whose
  facts changed compile again.

**C. Prebuilt std.** std's unit is checked, lowered and compiled once per toolchain and shipped:
interface, IR (for inlining and specialization) and objects. The toolchain is versioned as one,
so no resilience layer is needed. Packages are built once per version and cached.

An edit build then parses and checks the edited app unit against interfaces, lowers it, runs the
specialization stage (linear in the class table, incremental), compiles the functions whose IR or
facts changed, and links.

### Cache keys

A unit's entry key (`velt_build_cache::KeyHasher`, BLAKE3) covers everything its results depend
on: the compiler's identity; the unit's contents (std's files as read; a package's lockfile
integrity hash, plus the content hash of its files for a path or git dependency); the interface
hash of every unit it imports, in import order; the target triple, CPU and feature mask; the
options that change checking or code (release or debug, cell checks, debug info, backend,
`VELT_LLVM_OPT` for code only); and the build's conditions and the root package's `define`
constants. Because the key holds the imports' interface hashes and not their contents, an edit to
a unit's function bodies that leaves its interface unchanged does not change its dependents' keys.

A function's code entry records the facts its lowering consulted (is this type counted? does the
program read a `shared` value? the tag of this union member?) with their answers, and is served
only when every recorded question has the same answer in this program. Several entries, one per
distinct fact record, can live under one function. Class ids and union tags differ between
programs by construction, so cached code refers to them by symbol and the program defines the
symbols. A function whose fact record never repeats across programs (heavy `any` glue) stays in
the program.

An object and its fact record are one entry, so no build can pair one program's object with
another's record; entries are written atomically, so concurrent builds both succeed and leave one
valid entry. Nothing is ever stale: a new compiler, an edited std file, a package upgrade or a
changed program fact changes the key or the fact answers, and the entry misses.

### Soundness of served code

Serving cached code is correct only if lowering read every whole-program fact through the
recorded interface. Two guards:

- **Structurally:** lowering's program-wide state (counted types, the shared flag, tag and
  class-id tables, parameter attributes) is reachable only through that interface, so any fact a
  function's code depends on is recorded by construction.
- **Differentially:** a verify mode lowers and compiles every served function anyway and fails
  the build on any difference between the served entry and the fresh one, naming the function
  and the first differing fact or statement. CI runs it over all end-to-end tests in debug and
  release.

### Rules at unit boundaries

- **Slots and contracts.** Every non-static method of an exported std class has a slot in std's
  IR; nothing is sealed. The slot's contract is std's inferred one. A downstream override adapts
  through a thunk where its modes or errors differ: Borrow to Move copies, Move to Borrow drops
  after the call, Borrow to BorrowMut on a class receiver needs nothing (class values are
  references). The link devirtualizes slots nothing overrides.
- **Program-wide flags** (generator throws, frozen reads, writes of `undefined`, record
  accessors) do not exist across units: a unit is checked as if the flag's feature may be used,
  and the specialization stage specializes when it is not.
- **Augmentation** (`declare module`, `declare global`) keys the augmented unit: a program that
  augments a std interface gets that unit checked with the augmentation, cached per augmentation.
  Members are ordered by key, not load order.
- **`extend` resolves lexically.** An `extend … implements` must be in the unit of the interface
  or of the type (an orphan rule); overlap is an error where both are visible.
- **Representation variants** per function, keyed by the facts it read.
- **Error sets of function types.** A function-typed parameter without `throws` is
  error-polymorphic: the callee's set includes what the argument throws. A function type in a
  stored position (field, collection element, interface method) without `throws` in a library
  unit may throw anything, so a stored callback's call is a throwing call. No error set grows
  after its unit is checked.
- **Proxy traps** throw `unknown`, as in TypeScript. The `any` and proxy hooks are compiled in
  two variants keyed by the program fact "a trap that throws exists".
- **Union views.** A union whose members are interfaces always has its view variant in the
  declaring unit, so its layout is fixed by the unit.
- **Analyses that need the whole program to accept code** (replaced holders, absent fields,
  object copies, function-shape crossing) decide from the unit and its imports' contracts. Where
  the whole-program analysis accepted code, the unit form accepts it too and lowering uses the
  defensive representation; it never adds an error.

### Targets

- **Units in sema by default:** every end-to-end test passes with std checked as a frozen unit,
  the round loop runs per unit, and the oracle reports no differences.
- **The std unit cached:** the frozen unit's interface and checked bodies in the build cache;
  the app is checked against it without parsing std. On a body edit of `examples/todo_api.velt`,
  the front end takes at most 30 ms.
- **Unit IR and the specialization stage:** code cached per function by IR hash and facts read,
  with the verify mode in CI. A debug edit build of `examples/todo_api.velt` takes at most 100 ms,
  with run-time benchmarks within ±3%.
- **Prebuilt std and packages:** release builds by unit, inlining across units through exported
  bodies; a release edit build takes under 1 s.
- **Incremental inside the app**, and a resident `velt dev` that keeps units in memory and
  hot-swaps changed functions ([dev-warm-front-end.md](dev-warm-front-end.md),
  [hot-reload.md](hot-reload.md)): edit to running under 50 ms.
