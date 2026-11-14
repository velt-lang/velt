# Design: separate compilation (std and packages checked and compiled once)

Status: accepted (#2943; the owner's decisions D1-D6 on 2026-11-14). It replaces the order of work in
[package-cache.md](package-cache.md) and takes in #2501, #1728, #2189, #1612, #2930 and #2460 as
phases. Phase 0 is in progress.

## Problem

Velt compiles every program as one whole program: std, the packages and the app are parsed, checked
and lowered together, and each stage's result depends on all of them. The caches we have work at
the end of the pipeline (an object is reused when the lowered code comes out the same), so they can
never skip the front end. The front end is now most of an edit build, and every port of runtime
code to Velt makes it larger.

Measured on #2943 (a release `velt` at 8f4f1bc13, Apple Silicon, warm cache, a body edit in `main`):

| program | lines loaded (own) | `check` | debug, body edit | release, body edit |
|---|---:|---:|---:|---:|
| sync fs | 16.7k | 23 ms | 57 ms | 130 ms |
| async fs | 29.3k | 66 ms | 112 ms | 639 ms |
| `examples/apps/todo-api` | 69.9k (811) | 272 ms | 624 ms | 3,578 ms |

79% of todo-api's debug edit is the front end redoing std and http-server, which are 98.8% of what
it loads and didn't change. The executor adds 12.6k lines to every async program. Checking all of
std once (`velt check --std`) costs 1.3 s.

## The boundary rule

A *package* is the unit:
- a std module (`std/<area>/<module>/`; the prelude is one);
- a vpm package version;
- the app.

A package's **checked form** depends only on its sources, the **interfaces** of the packages it
imports, and the build options that change checking (conditions, `define`). Its **code** depends on
those and on the target and code options. Nothing an importing package does changes either one.

Whole-program facts stay, in two places only:
- the per-program **glue unit**, which cached code reaches by symbol: class ids and ranges, `any` and
  subclass dispatch tables, the app's vtables, the module-state layout;
- release optimization that is transparent to semantics.

## What each package produces

Each is cached by key, and std's is shipped prebuilt for the host.

1. **Interface (export data):**
   - declarations and checked signatures;
   - the summaries callers need: pass modes, write effects, error sets, `holds_promise`, fresh
     returns, async-ness, and each overridable slot's contract (D1).

   All identities in it are stable keys (below). Its hash is the **interface hash**: dependents are
   checked again only when it changes (early cutoff, as in Go).
2. **Checked bodies** of what callers instantiate or inline: generic functions, `inline()` and small
   functions, generator inputs. These are HIR in a versioned encoding, decoded on demand.
3. **Code:** an object per non-generic function. It is keyed by the checked body's hash and the
   lowering facts it was compiled under (its facts trace, package-cache.md), with variants per
   counted-type facts (D5).

## A program build

1. Check the app's modules against the decoded interfaces of what they import.
2. Lower the app's functions and the generic instances it needs. Instances are cached machine-wide
   by the generic's body hash and the type arguments' keys (#1612).
3. Generate the glue unit.
4. Link the app's objects, the cached package objects and the runtime.

Debug builds link std's code compiled optimized, which fixes #2460 for every port. Release builds
remove std's sema first, and adopt separate objects (with exported inline bodies, then ThinLTO-style
bitcode) only where run time stays within ±3% of whole-program code.

## Stable identity

`DefId` and `TyId` stay dense handles inside one compiler session. Everything cached or numbered
uses a **stable key** instead: the definition's path (`std/fs/files.velt::readFile`, member and
instance paths), as rustc's `DefPathHash` does.

- Class ids and static numbering go by key (#2930).
- Union tags take a canonical order by type key. The written order is kept for display and `keyof`.
- Module state is named per module.
- Encounter-order numbers become named statics.

## Decisions (owner, 2026-11-14)

- **D1 Dispatch contracts.**
  - A slot that can be overridden or implemented from outside its package has a contract fixed by
    that package: slot present, parameter modes, error set. Downstream overrides compile against
    it.
  - std classes and methods that aren't meant to be extended become sealed or final, which keeps
    today's direct calls.
  - The open ones (`Error`, `toString`/`toJSON`, the iteration and comparison interfaces) get
    conservative contracts: modes that admit a mutating override, and an error set that admits
    `Error`. That stays TS-compatible, with no new compile errors.
- **D2 Program-wide inference flags** apply to the package being checked. Imported packages are
  checked under one fixed, conservative setting.
- **D3 Interface augmentation** (`declare global`, `declare module`) stays allowed. The augmented
  package's key includes the augmentation, so a program that augments re-checks that package.
- **D4 `extend` resolves lexically:** a body sees the extensions in scope where it is written.
- **D5 Counted types:** cached code comes in variants keyed by the counted bits of the types each
  function touches. Phase 4 compares variant counts with one conservative representation.
- **D6 Unit:** a std module or a vpm package version.

## The std identity oracle

Sound caching needs a test, not an argument. `VELT_STD_SUMMARY=<file>` makes a check append a
hash per checked std definition to `<file>`:
- `velt_sema::interface::std_defs`;
- its interface hash, and the hash of its whole checked HIR with `DefId`s as names, `TyId`s as
  structural hashes and spans left out.

`bench/compile/std_identity.sh` compares every golden's std definitions with those of
`velt check --std` and names each that differs, and how. `VELT_STD_SUMMARY_SHOW=<part>` writes a
definition's hashed form, to diff the two.

A difference is a path by which a program changes std's checked form. Phase 2 removes them, and
phase 3 serves nothing until the count is zero, or each remainder is a keyed variant.

## Phases

0. **Measure.** `bench/compile/edit_loop.sh` (the table above, per stage), the oracle, and this note.
1. **Stable identity** (#2930): def keys; class, static and module-state numbering by key; union
   tags in canonical order; named statics for encounter-order numbers.
2. **Seal the boundary**, one change per path the oracle finds (D1-D4), with run-time benches
   within ±3%.
3. **Check std and packages once** (#2501): the HIR and interface encoding, checking against
   interfaces, the cache filled by an eager check, and shipped with the toolchain.
4. **Compile them once, debug first** (#1728, #2460): lowering reads program facts only through
   `Facts`, class ids and dispatch become symbols, and the glue unit. `VELT_PACKAGE_CACHE=verify` runs
   in CI.
5. **Release** (#2702): code units by package, inline bodies across packages, ThinLTO where objects
   cost run time.
6. **Packages and instances** (#2189, #1612), then a remote cache.
