# A warm front end for `velt dev`

Status: partly implemented. Issue: [#2621](https://github.com/velt-lang/velt/issues/2621).

## Summary

`velt dev` hot-swaps only the functions that changed ([hot reload](hot-reload.md)), but without
help it builds the whole program again first, on every save. Its JIT host is a long-lived process
that already holds the running version, so it keeps the work that led there too: the checked
context, the final lowering pass, the optimizer's per-function facts and the counted types. A
save that edits function bodies and nothing else is checked, lowered, optimized and verified for
the edited functions from that kept state, with everything else fixed. The output is exactly what
the cold front end produces for the same sources; any edit the warm path cannot prove it handles
takes the cold front end, which stays the reference.

The goal is that a save that edits the program's own code reaches the running program in under
100 ms for programs the size of `examples/apps/*`.

## Motivation

The cold front end is a pure function of the sources, run from scratch per save. Nearly all of
its work is std and the packages, which do not change during a session: on
`examples/http_hello.velt` (about 20 lines of program code over std and the `http-server`
package), sema's whole-program passes, `check_bodies` and lowering together take several hundred
milliseconds per save, while the program's own code is a few functions. `VELT_SEMA_TIMINGS`
prints sema's time per pass; `velt dev -v` prints the time of each stage of a reload.

## Design

### The model: library and app, interface and body

A program in `velt dev` has two parts:

- the **library**: std and every package (`SourceModule::is_std`, `is_dependency`), which
  essentially never changes during a session (an edit there, or of `package.velt` or the
  lockfile, takes the cold front end);
- the **app**: the program's own modules.

Every definition has an *interface*, what other code can observe of it (its signature, inferred
return type, error set, ownership pass modes, write effects, fresh returns), and a *body*. The
common save while iterating edits a function's body and nothing else. When a body edit leaves
the interface the same, nothing that depends on the definition has to be redone: passes that read
other definitions read their interfaces, which are the same by construction (early cutoff).

The host keeps everything in memory. Unlike the on-disk unit cache of
[build-architecture.md](build-architecture.md), nothing is serialized, so there is no encoder,
cache key or format version.

### When a save takes the warm path

Warm reloads are on by default in `velt dev`; `VELT_DEV_WARM=off` turns them off. Every cold build
in `velt dev` keeps its state for the next save. The warm path helps only while the program keeps
running: a program that exits is started again as a new process, with a full build.

A save takes the cold front end when:

- the change detection finds anything but body edits, a function it names is not found, or more
  than one declaration has its name (methods are named by kind: `T.get v`, `T.set v`,
  `T.static make`);
- what the source rewrites and scans find of the program differs from the kept check's: every
  rewritten module but the edited ones, whose trees the rewrites must leave as written, and the
  scans' and rewrites' side outputs, such as which generators are thrown into;
- the edit moves any other definition of its file (a body whose length changes, with definitions
  after it): the kept code of those definitions holds its old source positions, which lowering
  would resolve against the new text;
- an edited function's inferred result type or what it throws (its throw sources, by kind and
  type) changes: other bodies' checks read them;
- walking the live code again makes anything new live or demands a std module: the kept check
  loads no std and checks no std body late;
- the edited bodies report any diagnostic (warnings included, which the cold build then shows),
  or the reload needs another round of the throws sets, the sharing retry, or generated code the
  kept check did not generate;
- the warm path fails with an internal error (a panic): the kept state is dropped and the cold
  build keeps a new one.

After a warm check, the lowering may still be done whole (step 3E below lists when). A warm
reload reports what a cold one would: the warnings of the code the edit left alone, kept in the
context, are printed again on every reload.

## Compilation

The warm path is made of parts that each fall back to the cold front end when unsure. Step 2 is
the counted types; step 3 is the body-edit fast path, parts A to E.

### Step 2: the counted types carried over

Lowering lowers the whole program again while a pass finds more types that must be counted
(`lower/boxing/`), so a cold lowering is usually two passes, the first one only to find them. The
dev host keeps the counted types of each version (`velt_vir::CountedSeed`, in
`crates/velt_vir/src/lower/boxing/seed.rs`: types by their stable spelling `Cx::type_key`,
definitions by qualified name) and hands them to the next version's lowering
(`LowerOptions::counted_seed`, `velt_vir::lower_seeded`), whose first pass then already uses them
and is the only one when the edit counts the same types.

Counted sets only grow within a lowering, so a type an edit stops needing stays counted until the
host restarts: a representation the cold build would not pick, with the same behavior. The one
counted fact that is behavior, whether an `RwLock`'s `read` is exclusive (`lock_exclusive`), is
never seeded, nor the weak and proxy sets it is computed from; a seeded lowering that ends with a
lock value and any weak or proxy fact is done again without the seed. `velt build` and `velt run`
never use a seed. The same seed tells the host whether the running program's values fit the next
version's code (`CountedSeed::representation_change`, hot-reload.md "Classification").

### Step 3: the body-edit fast path

Per save:

1. Parse only the changed files (`velt_loader::reparse`, into the kept check's modules:
   `KeptCheck::reload_reparsed`) and compare each top-level definition with the running
   version's by its source text.
2. Check only the changed bodies, in the kept checking context, with every other definition's
   results fixed; then run the passes after the bodies' check.
3. Lower the functions of the edited files in the kept lowering context and splice them into the
   running version's program; verify and optimize only the changed functions, and hand them to
   the JIT, which compiles only what changed.

**A. Change detection** (`crates/veltc/src/dev/edits.rs`). Each top-level definition of the
program's files is kept as its *shape* (its source text with every function, method and
constructor body cut out) and the text of each body, by name. A save changed nothing, changed the
bodies of some functions and nothing else, or changed something else (a signature, a type, an
import, a definition added or removed), which takes the cold front end. Comparing source text is
conservative: a comment inside a body counts as a body edit, and one between definitions as
another change. A reload checks again the bodies changed since the kept check and those changed
since the running version (`located_since`): a body edited and then undone is the kept check's
text again, but the running version has the edit. `velt dev -v` reports what a save changed.

**B. Interface summaries** (`crates/velt_sema/src/interface/`). A hash per function of what other
code observes, and one per kind of program-wide fact a body can change, computed from the checked
HIR by structure and name (never by `TyId`, `DefId` or `Span`, which move when a body changes) and
compared between versions. After a body edit, `velt dev -v` says whether everything but the
edited functions and their closures kept its interface. A body edit that makes lazy std check
more definitions (the first `new Map` in a program) changes the program's reached std, the
declarations std's bodies assign, implementations and flags; such an edit takes the cold front
end. Facts sema keeps out of the HIR (which functions return fresh values) are not in the
summaries.

**C. A kept checking context** (`velt_sema`, used by the dev host only). The checked `Ctx` is
consumed and changed by the finishing steps (`crates/velt_sema/src/program.rs`: layouts merged,
readonly types erased, the definitions moved into the program), so the host keeps a clone made
before them.

- *C1, a cloneable context.* `Ctx` is `Clone` by derives: what it holds by value is copied, and
  the `Rc`s in it point to data that does not change. A clone shares what it borrows: the modules
  (unchanged) and the std modules loaded on demand (`StdLoads`, which only grows, so sharing it is
  sound). `VELT_SEMA_CLONE_CHECK=1` builds every program also from a clone and compares the two
  field by field (an `ICE` shows the difference), which shows that the clone holds what the
  finishing steps read and that nothing they change is shared
  (`crates/veltc/tests/clone_check.rs`). It does not show independence in general: a shared cache
  keyed by `TyId` passes it, since both builds intern types in the same order. Caches outside
  `Ctx` keyed by `TyId` or `DefId` (`locked_props::MEMO`) are reset by `module_state::build`,
  which every reload runs.
- *C2, an owning kept check* (`crates/velt_sema/src/kept.rs`). A `KeptCheck` holds the clone
  together with everything it borrows: the rewritten modules (the source rewrites' last copy),
  the loader's modules when it borrows them, the generated modules given with them, and the
  driver's `StdLoads`. The check offers it as a `PendingKept`; the driver hands over its vectors,
  and `PendingKept::adopt` takes them only if their heap addresses are the ones the clone
  borrows. An address cannot tell a vector from another allocated in its place, so `adopt` is
  `unsafe`, and its one caller says why the vectors are the checked ones. All the `unsafe` (the
  clone's lifetime erased, its drop first, `Send`, `adopt`) is in that one module.
  `borrows_only_what_it_owns` checks that every module the clone reads lies in a buffer the kept
  check owns. The clone shares no generator and loads no std. A check that added generated
  modules, or a script whose variables `script_locals` moved into `main` (another copy of the
  modules), keeps nothing.
- *C3, the reload* (`KeptCheck::reload`, `driver::compile_for_jit_warm` in
  `crates/veltc/src/driver/warm.rs`). The kept check holds the context from after the bodies'
  check (before the passes that follow it) and the final one; each reload clones the first, so
  nothing drifts from one reload to the next. In the clone, the edited bodies' first checks are
  undone together (`recheck::forget_all`), each body is given its declaration from the new text
  and checked again in the order of the first checks, the program's live code is walked again
  (`lazy_std::reach_again`), and the passes after the bodies' check run over the whole program
  (`kept_reload::reanalyze`). The refusals listed under "When a save takes the warm path" are in
  `crates/velt_sema/src/kept_reload.rs`.

**E. Splicing into the lowering** (`crates/velt_vir/src/lower/kept.rs`, `velt_opt`,
`velt_codegen_cl`). A dev build keeps its final lowering pass (`KeptLowering`), its optimized
program and what the debug optimizer learned (`velt_opt::DebugKept`). A warm reload's program is
spliced into the kept lowering: every instance of a function defined in a file edited since the
kept check or since the running version is lowered again, with what those newly reach, and only
the rebuilt functions come back (`Delta`). The delta is applied to the running version's
optimized program; only the changed functions are verified (`velt_vir::verify_changed`, with the
program-wide checks), optimized (`velt_opt::optimize_debug_delta`) and compared by the JIT
(`DevSession::reload_changed`, `Facts::update`, which fingerprints only the changed functions).

The splice is refused, and the program lowered whole (still from the warm check), when the type
table differs; an interface other than that of a closure of an edited file changes; lowering
learns a new fact (`boxing::Facts` only grow, so equal facts mean no other function lowers
differently); the counted types change; a task becomes pinned; lowering reports a diagnostic; or
an answer lowering keeps between functions changes. Each such answer has a rule
(`crates/velt_vir/src/lower/kept_answers.rs`):

| Rule | Answers |
|---|---|
| taken out, answered anew from the new program, and compared | about an edited function: whether it runs user code or is a known CPU check, an async function's state, a closure's environment, by-pointer params; about the whole program: what each function and direct callee stores, the fields, slots and globals code assigns, the params that may reach a function value |
| taken out and made again for the edited function alone | what only the edited files' functions name, such as an async function's promise wrapper |
| kept | the rest, which depends only on declarations or on std |

The delta optimizer refuses when a helper inlined into callers, or a numrep parameter fact, would
change. An internal error on the warm lowering builds the save cold. Every refusal keeps a new
lowering for the next save.

### Testing

`crates/veltc/src/dev/swap/warm_tests.rs` runs the whole warm path version after version, as the
dev loop carries it, and compares each program and its diagnostics with a cold build of the same
sources (aggregates, externs, statics and functions named by content): hand-written edits for
each refusal and random body edits of programs from the golden tests
(`warm_harness.rs`). `crates/veltc/src/dev/swap/tests.rs` checks that the spliced and
delta-optimized program equals a cold build's VIR and that the warm path parses nothing again;
`crates/velt_sema/tests/kept_check.rs` checks a kept check's ownership.

## Runtime cost

Measured on a 10,000-line typed application (50 features), release compiler, a same-length edit
of one feature's function while the program keeps running, 10 saves each, save to swap: warm
0.30–0.34 s (median 0.325 s), `VELT_DEV_WARM=off` 1.07–1.16 s (median 1.12 s). Most of a warm
save is sema's whole-program passes after the bodies (`ownership_passes` alone is the largest);
the splice, optimizer and verifier take tens of milliseconds each.

Memory: a kept check of `http_hello` (375 modules, about 5,500 definitions) raises the host's
resident memory from about 230 MB to about 370 MB in a debug compiler. Most of it is the clone and
the moment the original and the clone are both alive during the finishing steps; keeping the
original and making the finishing steps leave it intact would save both. A clone of that context
takes 11–15 ms in a debug compiler. `velt dev -v` reports the kept check's size and the process's
memory.

## Limitations

- An edit that changes a body's length moves the definitions after it in the same file, and takes
  the cold front end (most random edits anywhere in a file do).
- Any change to an interface, a declaration, an import, std or a package takes the cold front end.
- `velt build` and `velt run` are fresh processes and gain nothing from the warm path; the
  [unit cache](build-architecture.md) is their counterpart.

## Not yet implemented

- **D. Per-function passes.** The passes after `check_bodies` (ownership modes, throws, moves,
  exclusive access, the lock and thread checks) run for the edited functions with every other
  definition's results fixed, instead of over the whole program. This is most of a warm save's
  remaining time.
- **Shifted positions.** Definitions after an edited one in the same file keep their code, and the
  source locations they carry (panic messages, line tables, closure names) are shifted by the
  edit's change in length, so length-changing body edits take the warm path.
- **Warm parse for every edit.** The host keeps each file's AST keyed by its contents' hash and the
  parse options (conditions, `define`s), with one source map for the session so unchanged files
  keep their `FileId`s; a reload that takes the cold front end parses only files whose hash
  changed.
- **Reused lowered functions for every edit.** A function's VIR depends on its checked HIR, the
  types it mentions and whole-program facts (counted types, union tags, class ids, parameter
  attributes). Keyed by a fingerprint of its HIR and the facts it read, as the specialization
  stage of [build-architecture.md](build-architecture.md) keys machine code, a function whose key
  holds in the new program reuses its kept VIR.
- **The library checked once per session.** The library is checked once as its own unit, frozen,
  and kept; a reload checks only the app's modules against it, plus the library bodies the app
  newly reaches ([build-architecture.md](build-architecture.md) describes the unit split). Each
  whole-program pass states what it reads of other definitions, with a test comparing its output
  after an app edit with the cold build's; a pass that cannot state it stays whole-program.
- **No settle delay.** With content hashes, the host starts the build at the first change
  notification and restarts it if another write arrives, instead of waiting 30 ms
  (`watch::SETTLE`) every time.
- **Verify mode.** `VELT_DEV_VERIFY=1` also runs the cold front end on every reload and compares
  the swapped functions' VIR fingerprints and the diagnostics; the reload tests run in this mode
  in CI.
