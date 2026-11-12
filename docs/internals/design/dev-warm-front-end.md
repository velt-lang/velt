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

**2a, done: the counted types carried over (−90 to −110 ms).** Lowering lowers the whole program
again while a pass finds more types that must be counted: on `http_hello` two passes of about
100 ms each, the first one only to find them. The dev host keeps the counted types of each
version (`velt_vir::CountedSeed`: types by their stable spelling `Cx::type_key`, definitions by
qualified name) and hands them to the next version's lowering (`LowerOptions::counted_seed`,
`lower_seeded`), whose first pass then already uses them and is the only one when the edit
counts the same types. Measured in `velt dev` on `http_hello`: lowering per save from about
220 ms to about 110 ms. Counted sets only grow within a lowering, so a type an edit stops
needing stays counted until the host restarts: a representation the cold build would not pick,
with the same behavior; the one counted fact that is behavior, an `RwLock` whose `read` is
exclusive (`lock_exclusive`), is never seeded. Applying a seed spells every type of the new
version's table (`Cx::type_key`), which the measured win includes. `velt build` and `velt run`
never use a seed.

**2b: reuse the lowered functions.** Lowering builds every reached instance through a worklist,
then lowers again while the counted-types pass grows (`lower/boxing/`). A function's VIR depends on its checked HIR, the
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

### Step 3: the body-edit fast path (early cutoff)

The common save while iterating edits a function's body and nothing else. Every definition has an
*interface* (what other code can observe: its signature, inferred return type, error set, ownership
pass modes, write effects, fresh returns) and a *body*. When a body edit leaves the interface the
same, nothing that depends on the definition is redone. That cut-off replaces most of what 3c below
would have to prove pass by pass: passes that read other definitions read their interfaces, which
are the same by construction.

The fast path, per save:
1. Parse only the changed files (step 1) and compare each top-level definition with the running
   version's, by its source text: unchanged, a function whose body alone changed, or anything
   else (a signature, a type, an import, a definition added or removed), which takes the cold
   front end as today.
2. Check only the changed bodies, in the running version's checking context kept by the host
   (sema's `Ctx`, with the modules it borrows), with every other definition's results fixed:
   the per-function parts of the passes after `check_bodies` (ownership patching, moves,
   exclusive access, the lock and thread checks) run for those functions alone.
3. Recompute their interfaces. Unchanged: lower them in the running version's lowering context,
   splice them into its VIR, and hand that to the JIT (which already compiles only what
   changed). Changed: the cold front end.
4. Definitions after an edited one in the same file keep their code but not their positions: the
   source locations they carry (panic messages, line tables) are shifted by the edit's change
   in length.

What it needs, as separate pull requests, each falling back to the cold front end when unsure:
- **A. Change detection** (`veltc` dev host): per-definition source hashes and the edit's kind,
  reported by `velt dev -v`. No behavior change; the gate for the rest.
- **B. Interface summaries** (`velt_sema::interface`): a hash per function of what other code
  observes, and one per kind of program-wide fact a body can change, computed from the checked
  HIR by structure and name (never by `TyId`, `DefId` or `Span`, which move when a body
  changes), compared between versions. `velt dev -v` says after a body edit whether everything
  but the edited functions (and their closures) kept its interface (about 5 ms on `http_hello`;
  the fast path summarizes only the functions it checks again). What it showed: a body edit that
  makes lazy std check more definitions (the first `new Map` in a program) changes the program's
  `reached std`, declarations (fields std's bodies assign), implementations and flags, and the
  lock-free callbacks among std's closures. Such an edit takes the cold front end until C and D
  can check newly reached std bodies. Facts sema keeps out of the HIR (which functions return
  fresh values) are not in the summaries: D reruns `fresh_returns` over every widening, which is
  cheap.
- **C. A kept checking context** (`velt_sema`, dev only): the host keeps the running version's
  `Ctx` and modules; a body is checked again in it, with its closures' definitions replaced and
  its diagnostics its own. The checked `Ctx` is consumed and changed by the finishing steps
  (`program.rs`: layouts merged, readonly types erased, the definitions moved into the
  program), so the host keeps a clone made before them. In three pull requests:
  - *C1:* `Ctx: Clone`, by derives: what it holds by value is copied, and the `Rc`s in it point
    to data that doesn't change. A clone shares what it borrows: the modules (unchanged), the std
    modules loaded on demand (`StdLoads`, which only grows: sound to share) and the driver's
    generator, whose state generating code adds to. `VELT_SEMA_CLONE_CHECK=1` builds every
    program also from a clone and compares the two field by field (an `ICE` showing the
    difference). That shows the clone holds what the finishing steps read and that nothing they
    change is shared; it doesn't show independence in general (a shared cache keyed by `TyId`
    passes it, since both builds intern types in the same order). Every golden passes with it
    on. A clone of `http_hello`'s checked context (5,600 definitions) takes 11–15 ms in a debug
    compiler.
  - *C2:* an owning kept check (`velt_sema::kept`): the clone, the rewritten modules it
    borrows (the source rewrites' last copy), the loader's modules when it borrows them, the
    generated modules given with them, and the driver's `StdLoads`, in one value the dev host
    keeps (`KeptCheck`, with `-v` for now). The check offers it (`PendingKept`); the driver hands
    over the vectors and `adopt` takes them only if their heap addresses are the ones borrowed;
    an address can't tell a vector from another allocated in its place, so `adopt` is `unsafe`
    and its one caller says why the vectors are the checked ones. The `unsafe` (the clone's `'m`
    erased, its drop first, `Send`, `adopt`) is in that one module. `borrows_only_what_it_owns`
    checks that every module the clone reads lies in a buffer the kept check owns (a test fails
    without memcheck when an owner is dropped), and the tests run clean under memcheck. Kept: the first check of a program (round
    1) and a check with generated code offered from the cache, `http_hello`'s path. Not kept: a
    check in which a generator ran, and a script whose variables `script_locals` moved into
    `main` (another copy of the modules). Measured on `http_hello` (debug compiler, five
    saves): resident memory 230 MB without, 370 MB with a kept check of 375 modules and 5,474
    definitions. Most of it is the clone and the moment the original and the clone are both
    alive during the finishing steps, not the modules; keeping the original and making the
    finishing steps leave it intact would save both.
  - *C3:* a save that edits function bodies alone is checked from the kept check
    (`KeptCheck::reload`, `driver::compile_for_jit_warm`), and lowered as usual. The kept check
    holds the context from after the bodies' check (before the passes that follow it) and the
    final one; each reload clones the first, so nothing drifts from one reload to the next. In
    the clone the edited bodies' first checks are undone together (`recheck::forget_all`,
    against the lists as the records saw them), each body is given its declaration from the
    new text and checked again in the order of the first checks, the program's live code is
    walked again (`lazy_std::reach_again`), and the passes after the bodies' check run over the
    whole program (`reanalyze`). Warm sema is on by default in `velt dev` (`VELT_DEV_WARM=off`
    turns it off), so every cold build in `velt dev` keeps its check: two context clones per
    cold build, not only with `-v`. The warm path only helps while the program keeps running:
    a program that exits is started again as a new process, with a full build.

    A reload refuses, and the save takes the cold front end, when:
    - the change detection finds anything but body edits, or a function it names is not
      found, or more than one declaration has its name (methods are named by kind:
      `T.get v`, `T.set v`, `T.static make`);
    - what the source rewrites and scans find of the program differs from the kept check's
      (`kept_reload::Facts`: every rewritten module but the edited ones, whose trees the
      rewrites must leave as written, and the scans' and rewrites' side outputs, such as which
      generators are thrown into);
    - the edit moves any other definition of its file (a body whose length changes, with
      definitions after it): the kept code of those holds its old positions, which lowering
      would resolve against the new text. Rebasing them is left to step E;
    - an edited function's inferred result, or what it throws (its throw sources, by kind and
      type), changes: other bodies' checks read them;
    - walking the live code again makes anything new live or demands a std module: the kept
      check loads no std and checks no std body late;
    - the reload reports any diagnostic (warnings included, which the cold build then shows),
      needs another round of the throws sets, the sharing retry, or generated code the kept
      check did not generate;
    - the warm path fails with an internal error (a panic): the kept check is dropped and the
      cold build keeps a new one.

    Not done: checking the body in two clones and comparing them. Instead, a differential
    test (`dev/swap/warm_tests.rs`) compares warm reloads of random body edits of golden
    programs with cold builds (program and diagnostics). Caches outside `Ctx` keyed by `TyId`
    or `DefId` (`locked_props::MEMO`) are reset by `module_state::build`, which `reanalyze`
    runs.
- **D. Per-function passes** (`velt_sema`): the passes after `check_bodies` run for a set of
  functions with the rest fixed.
- **E. Splicing into the lowering** (`velt_vir`, dev only): the running version's lowering
  context lowers the changed functions (and instances they newly reach) into the kept VIR;
  locations after an edit shift.
- **F. Verification:** `VELT_DEV_VERIFY=1` also runs the cold front end and compares the
  swapped functions' VIR fingerprints and the diagnostics; the reload tests run in this mode in
  CI.

Expected for a body edit of `http_hello`'s handler: parse ~5 ms, check and passes a few ms,
lowering a few ms, JIT and swap ~15 ms: about 20–40 ms from save to the new response, against
about 450 ms now. Edits that change an interface keep today's path until 3a–3c below grow.

### Step 3a–3c: warm sema for every edit (expected −170 ms)

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
| lower + optimize | ~145 ms (one pass since 2a) | ~10 ms (changed functions) |
| diff, JIT, swap | ~15 ms | ~15 ms |
| **save to first new response** | **~540 ms** | **~65–75 ms** |

The same warm structures make `velt run` and `velt build` no faster (each is a fresh process);
package-cache.md is their counterpart, and steps 1–2 share its building blocks.

## Order of work and risk

0. Step 3's fast path A–F: the body edit, the common save, first.
1. Step 1 (warm parse): small, contained in `velt_loader` and the dev host; measurable alone
   (its `Words` fix for every build is in).
2. Step 2a (done), then the `Facts` interface and recorder (package-cache.md step 1) and
   step 2b.
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
