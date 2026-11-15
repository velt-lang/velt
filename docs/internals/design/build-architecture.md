# Design: units with interfaces (std and packages checked and compiled once)

Status: accepted direction (#2943, owner 2026-11-14), revised after M0's research and prototype
(2026-11-15). It replaces the order of work in [package-cache.md](package-cache.md) and takes in
#2501, #1728, #2189, #1612, #2930 and #2460.

## Problem

A Velt program is compiled as one inseparable unit at every stage, so a build costs
O(std + packages + app) whatever the edit. Measured on `examples/todo_api.velt` after a one-line
edit in `main` (debug build, dev-profile compiler, main at 740a98161): **1,027 ms**, of which 69k
of the 77k lines loaded are std.

| stage | ms | why it is redone |
|---|---:|---|
| parse | 120 | every std file is parsed again |
| sema | 355 | std's meaning depends on the program (below); ownership passes are 45% of it |
| lower | 344 | lowering reads whole-program facts, so all 3,511 functions are lowered again |
| optimize | 41 | |
| codegen | 89 | 3,510 of 3,511 functions come from the cache, which keys Cranelift IR: everything before it already ran |
| link | 67 | |

Why sema can't be cached: a function's checked meaning — ownership modes, error set, vtable slots,
representation — depends on code elsewhere in the program, std's on user code included. The
M0 inventory lists **83** such inferences and decisions (summary below). Sema also has fixed points
that discard the whole check and redo it, std included: grown error sets of function types, grown
union views, the sharing retry, lazy std bodies needed too late (`lib.rs`'s round loop).

Why moving the executor into Velt made builds slower: it turned a precompiled library into 12.6k
lines of std source that every async program checks, lowers and compiles on every build.

Output caches (#1612, #1728) can't fix this: their input is the whole program, and an edit changes
it.

## The rule

A **unit** is a std unit (std as one unit to begin with; finer splits later), a vpm package
version, or the app.

A unit's **checked form** depends only on its sources, the **interfaces** of the units it imports,
and the options that change checking. Its **IR** depends on those too. Its **machine code** depends
on its IR, the target and code options, and the program facts each function read (by key).
Nothing a dependent does changes any of these.

The rule is **enforced by structure, not sampled**: units are checked in dependency order, each to
completion, and a finished unit's definitions are frozen. The app's check reads std's interface and
cannot write to std's definitions. Every place that does today (the 83 below) fails loudly and must
take one of the forms below. Phase 2 as first planned — remove one dependency each time the oracle
finds it in a golden — was the wrong method: it is slow, and a clean sample proves nothing about
programs it didn't run.

Whole-program facts live in exactly two places:
1. **Program tables and the glue unit**: class ids and ranges, vtables and interface tables,
   `any` dispatch, module-state layout, per-program hook bodies (`Program::glue_bodies`),
   class objects.
2. **The specialization stage** (below): representation choices made from the facts of the linked
   program, keyed per function.

## A unit's interface

Modelled on what Lean (`.olean` borrow flags), Koka (`.kki` borrow info), Go (escape tags in export
data), GHC (`.hi` demand signatures and unfoldings) and Swift (module interfaces, SIL for
`@inlinable`) store. Each item has its own hash, so a dependent records exactly what it read
(GHC's per-declaration fingerprints, Zig's separate `func_ies` edges).

1. **Declarations and checked signatures**, with stable keys (`std/fs/files.velt::readFile`).
2. **A contract per function and per slot**, inferred bottom-up inside the unit:
   - parameter and receiver modes (Borrow / BorrowMut / Move);
   - the error set, in symbolic form: own errors ∪ the error variables of its function-typed
     parameters (Koka's effect polymorphism; std already writes `map<U, E>` by hand);
   - per parameter: retained, may cross threads, shares captures, may keep a borrowed function,
     may reach module state;
   - fresh-return, `holds_promise`, async-ness.
3. **Slots of every overridable member.** Every non-static method of an exported class has a slot in
   the unit's IR. Its contract is **the declaring unit's own implementation's**, not a conservative
   one (M0 below). A slot reference is symbolic; the link numbers it.
4. **Bodies for callers**: generic functions, `inline()` and small functions (getters, leaf
   functions, worker/wrapper entries), generator inputs, as versioned IR with an inline cost.
   Unfoldings are limited to what pays: Rust turns cross-crate inlining inference *off* under
   incremental builds because every exposed body turns body edits into dependent rebuilds.

## Three mechanisms

**A. Modular inference.** Ownership, error sets, sharing and thread facts are inferred per unit.
Fixed points stay inside a unit (or an import cycle within it). A dependent is checked against the
contracts and never changes them. Where it needs more, it adapts at its own boundary:
- an override or implementation whose natural modes differ from the slot's gets an **adapter thunk**
  in its own vtable entry (Swift's vtable and witness thunks);
- a closure passed where a parameter's contract doesn't allow its errors is checked against the
  contract (or wrapped, when the contract says "may throw anything");
- diagnostics follow from contracts: "this closure modifies captured `i`; `enqueue`'s callback may
  run on another task" is stated by std's signature, not by a whole-program walk.

**B. The specialization stage.** Units are cached as checked, lowered IR with symbolic references.
At link time a program-level stage, a ThinLTO-style "thin link" over summaries:
- numbers classes, slots, statics and module state;
- devirtualizes a slot every class of the receiver's family implements with the same method
  (LLVM's single-implementation devirtualization, Swift's effectively-final rule): Velt builds
  closed executables, so the link always knows the full class set;
- makes the representation choices that need program facts (counted types, views, identity
  flags, lock modes: D5);
- keys each function's machine code by (its IR hash, the facts it read), as HotSpot's CHA
  dependencies do at run time. Only functions whose facts changed compile again.

**C. Prebuilt std.** std's units are checked, lowered and compiled once per toolchain and shipped:
interface, IR (for inlining and specialization) and objects. The toolchain is versioned as one, so
no resilience layer is needed. Packages are built once per version and cached.

An edit build: parse and check the edited app unit against interfaces; lower it; run the
specialization stage (linear in the class table; incremental); compile the functions whose IR or
facts changed; link.

**Targets** (todo_api body edit): debug ≤ 100 ms (1,027 today); release < 1 s (3.6 s); a resident
`velt dev`, edit to running < 50 ms.

## M0 results

**Prototype of B** (branch `proto-devirt`, not for merge): `Error.name` given a slot in every
program (#2982), plus single-implementation devirtualization when lowering `Callee::Virtual`.
Release builds, instructions retired, best of 3–5:

| program | base (no slot) | slot, no devirtualization | slot + devirtualization |
|---|---:|---:|---:|
| hot `e.name` read, 50M | 718.4M | +294% | **717.6M (−0.1%)** |
| throw/catch, 3M | 518.7M | +10.3% | **518.7M (0.0%)** |
| hot `e.name = …` write, 10M | 43.7M | — | **277.6M (+535%)** |

Devirtualization at program level recovers the whole cost of dispatch. The remaining cost is the
*conservative contract*: the slot's setter has fixed modes, so it can't take ownership of its
argument and copies it. Hence the revision of D1: a slot's contract is the declaring unit's
inferred one, and an overrider that needs something else pays with an adapter.

**Inventory.** 83 whole-program inferences and decisions (sema and lowering, with paths, in the
#2943 thread). By modular form:
- **unit-local** (already, or once the round loop runs per unit): the throws worklist, the
  doubted-variables recheck, check-order hints, lazy std's live set, script locals, ~20 local passes;
- **contract in the interface**: parameter modes, inferred throws, dispatch error groups, slot
  modes and slots (D1 revised), fresh returns, crossing/locked/module-state facts per parameter,
  generic call-graph summaries for instantiation checks, "unavailable on target";
- **specialization stage**: counted types and box layout, tuples, views, weak, identity flags,
  lock modes, await steps, unwind pads, class ids, module state, symbols, `any` tables, upcast
  tables, class objects, per-program hook bodies, parameter attributes;
- **language decisions** (taken below).

## Decisions

The owner's D1–D6 (2026-11-14), with D1 revised by M0, and the decisions the inventory needs.

- **D1 Slots and contracts.** Every non-static method of an exported std class has a slot in std's
  IR; nothing is sealed (TS-compatible: any class can be extended). The slot's contract is std's
  inferred one. A downstream override adapts through a thunk where its modes or errors differ:
  Borrow→Move copies, Move→Borrow drops after the call, Borrow→BorrowMut on a class receiver is a
  reference write and needs nothing (class values are references). The link devirtualizes slots
  nothing overrides. `@open` (#2982) is not needed.
- **D2 Program-wide flags** (`gen_throw`, `frozen_reads`, `writes_undefined`, record accessors,
  …) don't exist across units: a unit is checked as if the flag's feature may be used, and
  lowering specializes when it isn't (a fact keyed per function, B).
- **D3 Augmentation** (`declare module`, `declare global`) keys the augmented unit: a program that
  augments a std interface gets that unit checked with the augmentation, cached per augmentation.
  Members are ordered by key, not load order.
- **D4 `extend` resolves lexically.** Coherence of `extend … implements`: an implementation must be in
  the unit of the interface or of the type (an orphan rule); overlap is an error where both are
  visible.
- **D5 Representation variants** per function, keyed by the facts it read (B).
- **D6 Unit**: std (split later), a vpm package version, the app.
- **D7 Error sets of function types at unit boundaries.** A function-typed *parameter* without
  `throws` is error-polymorphic: the callee's set includes what the argument throws (an implicit
  error variable, as std writes `map<U, E>` by hand). A function type in a *stored* position
  (field, collection element, interface method) without `throws` in a library unit may throw
  anything: a stored callback's call is a throwing call. No error set grows after its unit is
  checked. This removes the whole-program round of `throws::fn_types`.
- **D8 Proxy traps** throw `unknown` (as in TypeScript). The `any`/proxy hooks are compiled in two
  variants keyed by the program fact "a trap that throws exists", the second built only for
  programs that have one.
- **D9 Union views.** A union whose members are interfaces always has its view variant in the
  declaring unit (layout fixed by the unit). Measured in M1; if the cost is visible, opt in at the
  union's declaration.
- **D10 Analyses that matched by name or assumed "any function may run"** (`replaced_holders`,
  `absent_fields`, `object_copies`, function-shape crossing): their errors must be decidable from
  the unit and its imports' contracts. Where today's analysis needed the whole program to *accept*
  code, the unit form accepts it and lowering uses the defensive representation; it never adds an
  error the whole-program analysis didn't give.

## The std identity oracle

`VELT_STD_SUMMARY=<file>` makes a check write a hash per checked std definition.
`bench/compile/std_identity.sh` compares every golden's std definitions with those of
`velt check --std` and names each that differs, and how; `VELT_STD_SUMMARY_SHOW=<part>` writes a
definition's hashed form, to diff the two. `VELT_STD_SUMMARY_EAGER=1` checks every std file with the
program, as a cache of checked std would hold it, so a difference that only lazy checking makes (a
std body checked from one program's call sites) drops out and what is left is what the program
itself changes in std. Under the structural rule above the oracle becomes a regression test of the
frozen boundary, not the method of finding dependencies. Known gaps: it hashes `Program::defs` only
(no program tables), skips anonymous ADTs and union enums, and copies strings unchanged.

## Milestones

- **M0 (done):** inventory, research, the devirtualization prototype, this note.
- **M1 Units in sema.** Sema checks units in dependency order, each to completion; a finished unit is
  frozen (its definitions behind an immutable interface view); the round loop runs per unit. Behind
  `VELT_UNITS=1` until goldens, std tests and package tests pass. Each frozen-boundary violation is
  fixed by its form above, in this order: slots and contracts (D1), error sets (D7, D8), modes and
  sharing contracts, program flags (D2), the rest. Exit: every golden passes with std checked as a
  frozen unit, and the oracle reports zero.
- **M2 The std unit cached.** A versioned encoding of the frozen unit (interface, checked bodies) in
  the build cache; the app checked against it without parsing std. Debug builds and `check` use it
  by default. Exit: todo_api's front end on an edit ≤ 30 ms.
- **M3 Unit IR and the specialization stage.** Lowering per unit into IR with symbolic references;
  the specialization stage (numbering, devirtualization, D5 variants); code cached per function by
  (IR hash, facts read); std's objects reused. `VELT_PACKAGE_CACHE=verify` in CI. Exit: todo_api
  debug edit ≤ 100 ms with run-time benches within ±3%.
- **M4 Prebuilt std and packages.** Shipped with the toolchain; packages per version. Release builds
  by unit, inlining across units through exported bodies (#2702). Exit: release edit < 1 s.
- **M5 Incremental inside the app, and a resident `velt dev`** that keeps units in memory and
  hot-swaps changed functions. Exit: edit to running < 50 ms.

Each milestone reports `bench/compile/edit_loop.sh`'s table and the run-time benches.
