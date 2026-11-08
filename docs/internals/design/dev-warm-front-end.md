# Design: a warm front end for `velt dev` (hot reload phase 4)

Status: draft for review (#2621). Extends [hot-reload.md](hot-reload.md) ("Phase 4: keep the front end
warm") and borrows the interface model of [package-cache.md](package-cache.md) (#2189), in
memory instead of on disk.

## Problem

Developers expect a save to show in the running backend as fast as Vite shows it in the
browser: well under 100 ms. `velt dev` hot-swaps only the functions that changed, but it builds
the whole program again first, on every save.

Measured on `examples/http_hello.velt` (the `http-server` package, about 20 lines of program
code over std and the package), Linux x86-64, 4 cores, release `velt`, `velt dev -v`, a one-string
edit of the handler, save to the first response with the new body:

| step | time |
|---|---:|
| watcher settle delay (`watch::SETTLE`) | 30 ms |
| parse (std from the AST cache, the package and the program parsed again) | ~80 ms |
| sema (everything reached, std and the package included) | 190–250 ms |
| lower (the whole program) | 120–140 ms |
| optimize (debug pipeline, every function) | ~20 ms |
| diff, JIT of the 3 changed functions, swap | ~10–20 ms |
| **save to first new response** | **~540 ms median (498–581)** |

`velt run` after the same edit spends the same front end plus Cranelift and the link: about
560 ms. Of a reload's sema (`VELT_SEMA_TIMINGS`), `ownership_passes` is 127 ms, `check_bodies`
95 ms and `collect` 30 ms; lowering's time is spread over about 2,000 functions, none above
1 ms. Nearly all of that work is std and the package, which did not change: the program's own
code is a few functions.

The front end is a pure function of the sources, run from scratch per save. The dev host is a
long-lived process that already holds the previous version (its VIR and its JIT code) and
already decides per function what changed (`velt_vir::fingerprint`, `Mode::HotReload`). What it
does not keep is any of the work that led there.

## Goal

A save that edits the program's own code (not std, not a package, not `package.velt`) reaches
the running program in **under 100 ms** on the machine above, for programs the size of
`examples/apps/*`. The output is exactly what the cold front end produces: same diagnostics,
same VIR for the swapped functions. Anything outside the fast path falls back to today's full
build, which stays the reference.

## Model: the library is checked once per session

A program in `velt dev` is two parts:

- the **library**: std and every package (`SourceModule::is_std`, `is_dependency`), which
  essentially never changes during a session (an edit there, or of `package.velt`/the lockfile,
  starts a cold build, as today);
- the **app**: the program's own modules.

Today each save computes `F(library ∪ app)`. The warm host computes the library's part once,
keeps it, and per save computes only what the app's edit can change. Like package-cache.md, the
boundary is an **interface**: what the app may observe of the library (declarations, signatures,
layouts' inputs, and the summaries sema computes from bodies: ownership pass modes, error sets,
write effects, fresh returns, `holds_promise`). Unlike package-cache.md, nothing is serialized:
the host keeps the structures in memory, so no encoder, no cache key and no format version.

The hard part is the same in both designs: sema and lowering are whole-program today. Each step
below makes one stage incremental where it can be shown exact, and each ships on its own.

## Steps

Each step lists what it keeps, what it reruns, how it falls back and the expected saving.

### Step 1: warm parse (expected −30 ms, after a −25 ms fix for every build)

Measured inside the loader (warm caches, `http_hello`): the parse stage was ~89 ms, of which
decoding std's cached ASTs took ~22 ms, parsing the package's and the program's 53 files ~9 ms,
and `globals::mentions` (whether a module names one of the globals, a whole-source substring
search per module per name) about a quarter of loading. The last one is fixed for every build
by splitting each module's source into its words once (`globals::Words`): the stage is now
60–70 ms.

The host keeps each file's AST (after the loader's own processing) keyed by its contents' hash
and the parse options (conditions, `define`s). A reload neither decodes std's ASTs again nor
parses unchanged files; it parses only files whose hash changed, and rebuilds the module graph
from the kept ASTs. Spans carry a `FileId`, which a reload's new source map may number
differently: the host keeps one source map for the session (unchanged files keep their ids), or
re-targets a kept AST's spans.

- Fallback: a file not in the map is parsed, as today.
- Owner: `velt_loader` (a `ParsedFiles` map the caller may pass, used by the dev host only);
  `veltc` dev host keeps it in `DevSession`.
- Also: the whole-program source rewrites in front of sema (`module_chunks`, `nested_closures`,
  …; 11 ms here, parallel since #2605) take per-module results that can be kept the same way,
  keyed by the module's AST hash, once each rewrite is split into a per-module part and a
  whole-program part.

### Step 2: warm lowering (expected −110 ms)

Lowering builds every reached instance through a worklist, then lowers again while the
counted-types pass grows (`lower/boxing/`). A function's VIR depends on its checked HIR, the
types it mentions, and whole-program facts (counted types, the shared flag, union tags, class
ids, parameter attributes). package-cache.md's **facts trace** is exactly the record needed:
lowering asks those facts through one `Facts` interface, and a recorder keeps every question
with its answer.

The host keeps, per function key (symbol: stable across versions, hot-reload.md), the previous
version's VIR, its HIR fingerprint (a new `velt_sema` fingerprint of the definition's HIR and
the signatures it calls, like `velt_vir::fingerprint` one stage earlier) and its facts trace. A
reload's worklist reuses the kept VIR of an instance whose HIR fingerprint is unchanged and
whose trace holds in the new program; it lowers the rest.

- Fallback: any instance without a kept entry, or whose trace fails, is lowered as today.
- Verification: `VELT_DEV_VERIFY=1` lowers everything as well and compares the VIR
  fingerprints of the reused functions (debug aid and CI test mode).
- Shared work with package-cache.md: the `Facts` interface and recorder are its step 1; this
  step uses them without storage.
- The debug optimizer then runs only on lowered (not reused) functions: its passes are per
  function in the debug pipeline (−18 ms).

### Step 3: warm sema (expected −170 ms)

The most work and the biggest share. Sema checks the program in one `Ctx`, in rounds; its
whole-program passes (ownership modes, throws, moves, exclusive access) iterate over every body.

**3a. A library snapshot.** On the first build of a session the host checks the library on its
own once, with lazy std's live set seeded by what the app reaches, and keeps the resulting
`Ctx` (made `Clone`; it is `!Send`, which the host does not need). The library part of the
`Ctx` is laid out first: its `DefId`s, scopes, collected declarations and type-table entries
come before any app definition. Today the loader adds the root module first and the others in
discovery order (`velt_loader::load_program`), and sema collects in that order; the dev host's
build would order the library's modules first, and the cold build must check that the order
changes no diagnostic and no program (a test over the goldens).

**3b. Check the app against the snapshot.** A reload clones the snapshot and runs `collect` and
`check_bodies` for the app's modules only, plus the library bodies the app newly reaches (lazy
std's demand, which already works incrementally within a round). Library bodies already checked
are not checked again.

**3c. Incremental whole-program passes.** Each pass from `ownership_passes` on is made to run
over a set of definitions with the library's results as fixed inputs:
- *Ownership modes* (`infer_modes`): a fixpoint where modes only grow. Library functions'
  modes depend on library bodies and on function-typed parameters' modes, not on app code that
  calls them; start from the snapshot's modes and visit the app's functions plus any library
  function whose callee set gained an app function (a callback stored into library data). A
  library function whose mode would change invalidates the fast path (fallback).
- *Throws* (`throws::infer_all`, `fn_types`): library error sets are fixed; function types
  written without `throws` grow from the closures stored into them, which can be app closures
  (`FnSites`). If an app edit grows a library seed, fall back.
- *Per-function checks* (`moves`, `check_exclusive`, `check_locked`, `validate_moves`,
  `atomic_copies`, `thread_locals`, `struct_tuples`, …): run on the app's functions and on
  library functions instantiated with app types. They already do per-function work; #2605's
  `par.rs` showed which parts separate from the `Ctx`.
- *Instantiation passes* (`record_keys`, `json`, `void_fields`, `undefined_args`): over the
  instantiations whose type arguments mention app types.

Each pass states its dependency in a short table (what it reads of other definitions) with a
test that edits an app function and compares the pass's output with the cold build's. A pass
that cannot state one stays whole-program, which is correct and only slower.

- Fallback: a cold front end for the reload whenever 3c detects a library result would change,
  when the edit changes a declaration the library sees (an app type passed to a library generic
  gains a field: the instantiation is rechecked, not the snapshot), or on any error (errors
  are reported from the cold build, so their order and text never depend on the fast path).
- Verification: `VELT_DEV_VERIFY=1` also runs the cold sema and compares the HIR fingerprints of
  the app's definitions and the diagnostics.

### Step 4: the settle delay (expected −20 ms)

`watch::SETTLE` waits 30 ms after a change for an editor's multi-step save to finish. With
content hashes from step 1, the host can start the build at the first notification and restart
it if another write arrives (the build is cheap by then), instead of waiting every time.

## Expected result

| step | today | warm |
|---|---:|---:|
| settle | 30 ms | ~5–10 ms |
| parse | ~80 ms (60–70 after the `Words` fix) | ~5 ms (the edited file, the graph) |
| sema | ~200 ms | ~20–30 ms (app bodies, incremental passes) |
| lower + optimize | ~145 ms | ~10 ms (changed functions) |
| diff, JIT, swap | ~15 ms | ~15 ms |
| **save to first new response** | **~540 ms** | **~65–75 ms** |

The same warm structures make `velt run` and `velt build` no faster (each is a fresh process);
package-cache.md is their counterpart, and steps 1–2 share its building blocks.

## Order of work and risk

1. Step 1 (warm parse): small, contained in `velt_loader` and the dev host; measurable alone
   (its `Words` fix for every build is in).
2. The `Facts` interface and recorder (package-cache.md step 1), then step 2.
3. Step 3a–3b (`Ctx: Clone`, library-first layout, app-only collect/check) with the full
   whole-program passes still run: already removes `check_bodies`' library share.
4. Step 3c pass by pass, largest first (`ownership_passes`: 127 ms here), each behind its own
   dependency test.
5. Step 4.

Risk is concentrated in step 3: a missed dependency is a wrong program in dev only, never in
`velt build` (which never takes the fast path), and `VELT_DEV_VERIFY` plus the reload test suite
(`tests/reload/`) running in verify mode in CI is the guard. Each step falls back to the cold
front end, so an incomplete step costs speed, not correctness.

## Measuring

`bench/reload` gains the measurement above for `examples/http_hello.velt` and one larger app
(`examples/apps/shop-api`), reporting save to first new response and the per-stage times, run
before and after each step.

## Open questions

- Does any sema pass read library bodies in a way that depends on the set of app definitions
  without going through a call or a type (for example counts over all definitions)? Each such
  read must become a stated dependency or keep that pass whole-program.
- Memory: the snapshot doubles the host's front-end memory at worst (one clone per reload,
  freed after it); measured on `sigx` before step 3 ships.
- The program's own modules besides the edited one: step 3 treats every app module as edited.
  A finer split (per app module, like package-cache.md's later work) is only worth it for large
  apps.
