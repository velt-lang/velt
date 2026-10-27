# Design: compiling std's Velt code once (a cached std object)

Status: proposed (#1728, part of the JSON engine, #1655). Nothing here is implemented.

## Problem

Std code written in Velt is compiled again inside every program that uses it. Code that lived in
the Rust runtime was compiled once, when Velt was built, and only linked (`libvelt_rt.a`,
[Linking](../linking.md)). Each move of std code from Rust to Velt
([json-engine.md](json-engine.md), "the engine is written in Velt") so adds compile time to
every program that reaches it.

The issue's example is number formatting: the JSON engine moves Ryu and JavaScript's layout
rules from the runtime into Velt (#1658). A release build of

```ts
function main() {
  const x = process.argv.length * 1.5;
  console.log(`x=${x}`);
}
```

emits 3,401 lines of LLVM IR instead of 239 for the integer version. The formatter has one call
site, so the inliner moves it into `velt_main` and deletes the original
(`crates/velt_opt/src/inline/mod.rs`: an internal function with one call site is inlined up to
`ONCE_COST` = 2,000). Nothing is duplicated; the formatter is compiled once, but once per
program. `--timings` on an Apple M4:

| | VIR optimize | LLVM codegen |
|---|---:|---:|
| integer in the template | 19 ms | 57-64 ms |
| float in the template | 20-27 ms | 112-114 ms |

That is about +55 ms of codegen, plus the optimizer's time, in every build that prints a
non-integer number. The JSON scanner, reader, writer and tape pay the same per program, and so
will every later port. Debug builds pay too, and get std's code unoptimized: Cranelift with
`OptLevel::None` (`crates/veltc/src/driver.rs`, `compile_to_vir`), which the JSON engine's
gate already worries about (json-engine.md, step 0, "debug builds too").

## Proposal in one paragraph

The compiler builds std's non-generic functions into objects, optimized, once per
toolchain, target and CPU, and keeps them in a cache. A program still lowers the std functions it
reaches, as today. Before optimizing, it compares each one's VIR with the VIR the object was
built from. Where the two match, the function's body is dropped and its calls become ordinary
calls to a symbol that the object defines; the linker adds the object as it adds the runtime.
Generic functions, small functions and functions std marks as fast paths stay in the program,
where the inliner can use them. The code in the object is still Velt compiled by Velt, so the
rule "no Rust fast paths" holds. Debug builds get std optimized.

## What stays in the program, and what goes to the object

| Std function | Where its code is | Why |
|---|---|---|
| Generic (`function f<T>`, methods of generic classes) | Program, one instance per type, as today | Instances depend on the program's types |
| Caller-tracking (`track_caller.rs`: small ones get an instance per call site, larger ones take the caller's location as a parameter) | Program | An instance per call site is the point of the small ones |
| Marked as a fast path (`__intrinsic_inline()`, below) | Program | Callers must be able to inline it, such as the integer path of number printing |
| Optimized cost ≤ `SMALL_COST` (40, `inline/mod.rs`) | Both: the object has it, the program keeps its body to inline it | Inlining a small function usually shrinks the caller |
| Every other non-generic function | Object | The cost this note removes |
| One whose VIR in this program differs from the object's (next section) | Program, as today | The object's code would be wrong for this program |

A fast path is marked with a statement at the top of its body, as gated functions use
`__intrinsic_target("sha-arm")` (`std/crypto.vlt`):

```text
export function numberToString(x: number): string {
  __intrinsic_inline();            // std only: compiled into each program, inlinable
  if (isSmallInteger(x)) return intToString(x);
  return formatDouble(x);          // large: called in the std object
}
```

Sema accepts it only in std files, as it does every intrinsic (`body/expr/builtins.rs`). Once
compile-time decorators exist (#1664), it can become a decorator.

**Inlining across the boundary.** A function in the object is never inlined by the rule that made
the example big (one call site, up to `ONCE_COST`), since that rule exists because the original is
deleted afterwards, and here nothing is deleted. Small functions still inline (their bodies stay).
The loop rule (`LOOP_COST` = 1,000: a callee called from a loop inlines while the budget lasts)
does not reach object functions in this design. That loss is what the run-time gate (3%)
measures; where it shows, the fix is a marked fast path, not a higher threshold for all
(open question 3).

## Is the object's code right for this program?

Std is not compiled the same way in every program. Lowering and sema decide some things for the
whole program, and they decide them differently when the program adds classes or callbacks:

| Whole-program fact | Where | Effect on a std function |
|---|---|---|
| Class ids for `instanceof`: a pre-order walk of all classes, in definition order | `velt_vir/src/lower/class_test.rs`, `number_classes` | The constants in a range check |
| Whether a class hierarchy has a vtable pointer (a subclass or a virtual method anywhere) | `lower/layout.rs`, `has_header` | Field offsets of a std class a user extends |
| `any` tables: slots present "when the program has them", one table per type, compared by address | `lower/any.rs` | Table contents and identity |
| Error sets of function types written without `throws` grow with the program's callbacks | `velt_sema` (the repeated check, internals README step 3) | The `Result` layout of a std function that takes a callback |
| Module state: one block, slot offsets for the whole program | `lower/module_state.rs`, rt_abi.md "Module state" | The slot offsets of a std module-level `let` |
| Proxies present in the program | `cx.has_proxies()` in `class_test.rs` | Extra paths for `instanceof` |

Listing such facts and checking each would be fragile: the next one added to lowering would
break the object silently. So the check compares results instead of facts:

1. The object build records, per function, a **fingerprint**: a hash of its lowered VIR, before
   optimization, in a canonical form. Callees, statics and aggregates appear by content and
   symbol, not by their `FuncId`/`StaticId`/`AggId` numbers. Source locations and debug
   variables are left out, since they do not change what the code does.
2. A program lowers the std functions it reaches, as today, and computes the same fingerprint
   for each.
3. A function **links** to the object when its fingerprint equals the object's and every function
   it reaches in the object (direct callees, functions in its statics' relocations, its
   closures and glue) links too. That is a fixpoint over the program's call graph: one
   mismatch, such as a class id that moved, takes the function and its callers in the object back
   into the program.
4. A function that links becomes a declaration (below). Functions only it referenced (its
   closures, glue, internal helpers) are dropped by `dead_funcs`, as they live in the object.

A mismatch costs speed, never correctness: the function is compiled in the program, exactly
as today. A program that extends `Error` keeps the `Error` functions that touch its fields in
the program; one that does not, links them. `VELT_STD_OBJECT=verbose` lists the functions that
did not link and why (fingerprint, or which callee).

The check needs lowering, but not optimization or codegen, of std functions. Lowering was not
in the issue's measurements; the first pull request measures it (`--timings`, stage `lower`).
If it turns out to matter, a cheaper fingerprint over HIR can replace it later (open question 5).

**Identity of statics.** `any` tables are compared by address ("a checked conversion compares one
pointer"), and the runtime compares some statics' addresses (type descriptors;
`velt_codegen_llvm/src/units/mod.rs`, which already gives them one copy across codegen units).
A function that refers to such a static does not go into the object in the first version: two
copies, the program's and the object's, would compare unequal. The object build marks these
functions. A later version can give such statics one symbol, defined once, as codegen units do.

## Symbols and linkage

- **Names.** An object function keeps its mangled name (`crates/velt_vir/src/mangle.rs`:
  `std/fs::readFile` → `_V3stdP2fsN8readFile`). These names contain no `TyId`s, so they are the
  same in every program. Runtime symbols start with `velt_rt_`, so the two never collide.
- **Visibility.** The object defines its exported functions `hidden`: visible to the static link,
  never exported from the executable. Its own closures, glue and helpers are `internal`. A
  program that compiles a function itself (a mismatch) defines it `internal`, so its copy and
  the object's never clash.
- **In VIR.** A program's linked std functions become `Function`s with a new
  `Linkage::Import`: a signature, param attributes, and either no blocks or a body kept only for
  inlining (small ones and marked ones). Backends never emit an `Import` body. LLVM declares it as
  codegen units already declare a function another unit defines (`units/mod.rs`,
  `module.rs`, `function_declaration`: `declare hidden … ` with its param attributes), and
  Cranelift as an `Import` with `colocated`, a direct call. A `vir::ExternFn` would also work,
  but it has no param attributes, and the inliner could not see a body. `Linkage` is in `vir.rs`,
  a contract: maintainer review.
- **Calling convention.** Both backends give every VIR function the target's C convention on
  scalar-only signatures (`velt_vir/src/lib.rs`, "Calling convention"; `velt_codegen_cl/src/abi.rs`,
  `velt_codegen_llvm/src/module.rs`). So a Cranelift debug program calls an LLVM-built object
  directly. The places where the two backends could differ (`i8`/`bool` extension, `f32`,
  out-pointers) get a test (Testing).
- **Caller facts.** An object function may be called by any program, so the object build treats
  it as having unknown callers. It gets no constant-parameter facts from `numrep`
  (`numrep/params.rs` already applies them to internal functions only), and no ONCE inlining
  into its single caller. The param attributes it relies on come from its signature (pass modes,
  `ParamAttrs` from lowering), which is part of the fingerprint.

## Building the object

`velt` builds the object itself, the first time a build needs it and finds none:

1. Load the prelude and every std module, as `bench/compile/all_std.vlt` does, and check them.
2. Lower with a root set instead of `main`: every non-generic, non-caller-tracking, unmarked
   std function, plus what they reach. This is a new mode of lowering (today "only functions
   reachable from `main` are lowered"; `velt_vir/src/lib.rs`). Record each root's fingerprint.
3. Optimize with `OptLevel::Speed`, roots kept (`dead_funcs` keeps them as it keeps `Export`
   functions). Record each root's optimized cost.
4. Generate code with LLVM when clang is available, at `-O3`, with line tables (`-g` line tables
   only, so backtraces and debuggers show std lines). Release links strip them, as they strip
   the program's. Without clang, Cranelift with `optimize: true`. One object per std module (the
   prelude counts as one), using the codegen-unit machinery: parallel clang processes, functions
   called across members `hidden`.
5. Write the objects and a `manifest.json`: per exported function its symbol, fingerprint,
   optimized cost and member, plus the key below and the std content hash.

The build uses all cores. A program links only the members whose functions it calls, plus the
members those call (from the manifest). No archive tool is needed, which matters on Windows,
and a debug link, which does not collect unused sections, pulls in no unused modules.

Std's file paths appear in panic messages (`lower/srcloc.rs`, `panic_suffix`). For the
fingerprint, and a prebuilt object, to be the same in every install, the object and the
program must write std paths relative to the std root (`std/json/scan.vlt:12:3`), not as the
absolute path of one machine. That is a small change of its own, first in the order of work.

## Cache key

The object depends on everything that changes its code without changing a fingerprint:

| Part | Value | Why |
|---|---|---|
| Compiler | Release: the version and commit. Source checkout: the `velt` executable's size and modification time, as the link stamp does for the runtime (`crates/veltc/src/link.rs`, `link_key`) | Lowering, the optimizer and the backends change between compilers |
| Runtime | The runtime library's path, size and modification time | The object calls `velt_rt_*`, and inlines layouts the runtime defines (rt_abi.md) |
| Std | A hash of every file under the std root | Not needed for correctness (fingerprints are), but an edited std should rebuild the object, not leave it unused |
| Target | The normalized triple | |
| CPU | The resolved `--cpu` / `$VELT_CPU` / `build.cpu`, and its x86 feature mask (`Backend::required_features`) | `--cpu baseline` must not run code built for the default CPU; the start-up CPU check (`driver/cpu_check.rs`) covers the program's code and the object's |
| Code generator | `llvm` with `$VELT_LLVM_OPT`'s level, or `cranelift` | |
| Lowering options that change VIR | `cell_checks` (on in programs a debug compiler builds) | |

Not in the key: the program's profile, its backend and `-g`. One object serves debug and release
builds, Cranelift and LLVM programs.

The key's hash names a directory: `<cache>/std/<key>/` with the members and `manifest.json`.

## Where the cache lives

`$VELT_CACHE_DIR` when set; else `$VELT_HOME/cache/std/`, beside the package cache vpm keeps in
`$VELT_HOME/cache` (`crates/vpm/src/locations.rs`; default `~/.velt`). One home for what Velt
downloads and builds keeps `velt doctor`'s check that `VELT_HOME` is writable
(`commands/doctor/checks.rs`) meaningful, and gives users one directory to delete. The issue
suggests `~/.cache/velt`; that is open question 2.

A read-only or missing cache directory is no error. The build compiles std in the program, as
today, with a one-line note under `-v`.

**Invalidation** is by key: a new compiler, runtime, std or CPU makes a new directory, and the old one
is never used again. **Eviction:** each fill touches `<key>/used`, and after a fill the cache
keeps the eight most recently used keys per target and deletes the rest. In a source checkout every
rebuild of `velt` is a new key, so this bound matters there most. `velt clean --cache` empties it.

## Two builds filling the cache at once

The golden tests start hundreds of builds at once on a cold cache. Without care, each would
build the object.

1. A build that finds no `<key>/manifest.json` takes an exclusive lock on `<cache>/std/<key>.lock`
   (`std::fs::File::lock`: `flock` on Unix, `LockFileEx` on Windows).
2. Holding it, it checks again, since another build may have finished meanwhile. Then it builds into
   `<key>.tmp-<pid>-<random>/`, writes `manifest.json` last, and renames the directory to `<key>/`.
   A rename within one directory is atomic on every platform; when `<key>/` already exists
   (a build that did not see the lock, such as one on a network file system without locks), the
   rename fails, and the build deletes its copy and uses the other one.
3. Other builds wait on the lock, then read the finished directory. A build that cannot take the
   lock within 60 seconds compiles std in the program, as today.
4. Readers trust only a directory with a `manifest.json`, which is written last. A crashed fill
   leaves a `.tmp-` directory that the next fill deletes once it is older than an hour.

## Profiles, backends and commands

| Build | Std code |
|---|---|
| `velt build --release` (LLVM or Cranelift) | Object |
| `velt build` / `velt run` (debug, Cranelift) | Object, so std is optimized. Debug builds link the shared runtime (`link.rs`); the object's runtime calls resolve against it as the program's do |
| `--backend llvm` debug | Object |
| `velt dev` with the JIT host | In the program, as today (first version). The host JIT-compiles in memory and resolves symbols from a table (`velt_codegen_cl/src/dev/version.rs`); it cannot load an object file. Later the host can compile the std part once when it starts and keep it across reloads (hot-reload.md, "keep parsed and type-checked std") |
| `velt dev` restarting a built program | Object |
| `velt test` | Object |
| `--emit obj` | In the program: the object is promised to be self-contained, as today (cli.md) |
| `--emit vir`, `--emit llvm` | As `exe` would build: linked functions appear as declarations. `VELT_STD_OBJECT=0` shows std's code |
| `velt check`, `velt fmt`, `velt lsp`, `velt doc`, the playground's checks | Never build or read the object: they do not reach codegen (`driver.rs`, `check` stops after sema) |

`VELT_STD_OBJECT=0` compiles std in the program everywhere: the escape hatch, the A/B switch for
benchmarks, and the mode the inline test runs use.

**Debugging std code.** A debug build steps into std's code optimized, with line tables but no
variables, as Rust's std behaves. `VELT_STD_OBJECT=0` gives the old debuggable copy.

## Targets: wasm32 and cross

- **wasm32** (`wasm32-wasip1` and the browser flavor) needs the LLVM backend already (`driver.rs`
  rejects Cranelift for it), so its object is built with `opt` and `llc` (`velt_codegen_llvm/src/llc.rs`),
  like its programs, on first use. `wasm-ld` links object files as other linkers do. Without
  LLVM tools there is no wasm build at all, so no new requirement.
- **Cross targets.** A non-host target's runtime and link kit come with its target pack
  (`velt target add`, `lib/targets/<triple>/`, `velt_link/src/lib.rs`, `find_runtime_lib`). Its
  object is built on first use, with clang for that triple, or with Cranelift for a debug build
  without clang. Both already produce the target's program objects, so nothing new is needed.
- **Prebuilt in target packs.** The packs `scripts/package.sh` makes can carry the object too
  (below).

## Prebuilt in the installed toolchain

`scripts/package.sh` builds the object for the host target and the default CPU with the
just-built `velt`, LLVM, and puts it in `<prefix>/lib/std/<key>/`, as it puts `libvelt_rt.a`
in `<prefix>/lib/`. Target packs get theirs in `lib/targets/<triple>/std/<key>/`. The lookup
order is:

1. `$VELT_STD_OBJECT_DIR`, a directory holding one key's directory (tests, and installs that keep
   it elsewhere);
2. the toolchain's `lib/std/` or the pack's `std/`, used when the key matches;
3. the cache, filled when empty.

The toolchain's key uses the release version, not the executable's time, so a prebuilt object
matches the `velt` it shipped with. An edited `<prefix>/std` changes the std hash, so the
prebuilt object is not used and the cache gets one. Users with no clang still get LLVM-optimized
std code from the prebuilt object. The size it adds to a release is measured in the
pull request that ships it.

## Linking

The link step (`crates/veltc/src/link.rs`) passes the needed member objects after the program's
objects and before the runtime: they call the runtime, and the program calls them. The link stamp
(`link_key`) hashes their paths and contents' identity, as it does the runtime's, so a new
object relinks.

## Testing

| Test | What it checks |
|---|---|
| Goldens with the object (the default) | Every golden, debug and release, as today |
| Goldens with `VELT_STD_OBJECT=0` | The nightly run, and in pull requests the `m1` goldens (as `llvm_goldens.rs` runs them through LLVM); a change under `crates/velt_vir`, `crates/velt_opt` or `std/` runs them all |
| Same output both ways | A test builds a set of programs (number printing, JSON both ways, a class extending a std class, a callback that throws into std, `instanceof` on std classes, async std I/O) with and without the object and compares output and exit codes |
| Fingerprints | Unit tests in `velt_vir`: the canonical form is the same under renumbered `FuncId`s, `StaticId`s and `AggId`s, and differs when a class id constant, a field offset or a callee changes |
| Mismatch falls back | A program whose `class E extends Error` changes `Error`'s layout keeps the `Error` functions in the program (`--emit vir` shows bodies) and prints the same as with `VELT_STD_OBJECT=0` |
| Calls between backends | A Cranelift debug program calls LLVM-built object functions with `bool`, `i8`, `u16`, `f32` and `f64` parameters and results, aggregates by pointer, an out-pointer `Result`, a closure argument |
| Concurrent fills | Sixteen `velt build` processes on an empty cache: all succeed, one directory results, no `.tmp-` left |
| Commands that must not build | `velt check`, `velt fmt`, `velt lsp` (one request), `velt doc` leave an empty `$VELT_CACHE_DIR` empty |
| Install layout | `install_layout.rs` runs from a prefix with a prebuilt object and checks it is used (no cache directory created) |
| Fuzzes | `bench/json/fuzz/run.sh` (numbers, strings, values) in both modes |

Tests that look at std's code in VIR (the `opt_*_vir.rs` tests, `forward_consts_vir.rs`) set
`VELT_STD_OBJECT=0`, or keep the functions they look at in the program.

## Acceptance check

From the issue, each measured on the same machine, base (`main` at the merge base) against
head, with interleaved runs:

| Check | Target | How it is measured |
|---|---|---|
| The float example compiles within 5% of the integer version | `velt build --release` wall time, float ≤ 1.05 × integer | `velt build --release --timings` best of 20 for each program, warm cache, total of all stages. The LLVM IR line count (`--emit llvm`) goes in the pull request too |
| No compile-time regression for programs with no std Velt code | Within noise (3%) | `bench/compile/run.sh` (`compile_speed`) as today, plus a codegen column: the script times `--emit vir` only, so the pull request adds `velt build --release -v` totals for `http_hello` and the generated programs |
| Run time | Every row within 3% | `bench/json/run.sh` and the number fuzzes' timing programs (instructions retired: cachegrind on Linux, `/usr/bin/time -l` on macOS, as json-engine.md's gate), `bench/compiler_ab.sh BASE HEAD` for `bench/typical` and the other programs |
| Debug speed | Reported, no target | JSON benchmarks in a debug build, before and after (expected: much faster) |
| First build on a cold cache | Reported | Time to fill the cache for the host target, cores used |

## Contracts

- `vir.rs`: `Linkage::Import`, a function defined in the std object (maintainer review).
- `velt_vir`'s lowering entry: a root set instead of `main` for the object build, and the
  fingerprint function.
- HIR: the `__intrinsic_inline` intrinsic (hir_encodings.md).
- cli.md: `VELT_STD_OBJECT`, `VELT_STD_OBJECT_DIR`, `VELT_CACHE_DIR`, `velt clean --cache`;
  `--emit vir|llvm` show declarations for linked std functions.
- rt_abi.md: nothing changes. The object calls the runtime as programs do.

## Alternatives considered

- **Caching per function, incremental compilation.** Cache each function's object code under a
  hash of its VIR, for std and user code alike. It is more general, but each function becomes its
  own object (link time and size grow), the hash must cover inlined callees, and the cache churns
  with every edit. Std is the part that does not change between builds, so caching it whole
  gets most of the gain for a fraction of the machinery. Per-function caching can come later,
  using this note's fingerprints.
- **ThinLTO summaries.** Ship std as ThinLTO bitcode with summaries and let LLVM import what
  each program needs. Only the LLVM backend could use it (debug builds and the JIT are
  Cranelift), it needs `lld` with LTO at every link, and each link runs LLVM's optimizer on the
  imported code: the codegen time this note removes comes back at link time.
- **Shipping std as LLVM bitcode.** Same problem without the summaries: the program's clang run
  still optimizes and generates code for the std it uses. It saves Velt's front end, which is not
  the cost here.
- **Keeping a Rust copy.** Compile std's hot parts in Rust, in the runtime. That is what the JSON
  engine set out to remove (json-engine.md, "the engine is written in Velt"): calls the program
  cannot inline, a second implementation to keep in step, and a proof only that Rust is fast.
- **Turning off the ONCE inlining rule for std.** It would stop the example's formatter from being
  inlined, but the formatter would still be optimized and compiled in every program, just not
  inlined.

## Order of work

1. Std paths relative to the std root in panic messages and debug info.
2. VIR fingerprints and `Linkage::Import` in both backends (tests with hand-built VIR).
3. The object build and the cache (lock, rename, eviction), off by default
   (`VELT_STD_OBJECT=1` to try). Goldens in both modes.
4. `__intrinsic_inline`, the number formatter split into its fast path and the rest.
   Measurements of the acceptance check; then on by default.
5. Prebuilt objects in `scripts/package.sh` and target packs.
6. Later: statics with identity in the object, the JIT host keeping std compiled, a cheaper
   fingerprint if lowering std shows up in profiles.

## Open questions

1. **Which functions go in the object?** Everything non-generic above `SMALL_COST`, as proposed,
   or a higher threshold (say 200), so that more of std stays inlinable at the price of more
   per-program codegen? **Recommendation:** `SMALL_COST`, with fast paths marked by hand, since
   the cost model's own boundary needs no new constant to tune, and marks name the cases
   that matter.
2. **Where is the cache?** `$VELT_HOME/cache/std` (beside vpm's package cache) or the platform's
   cache directory (`~/.cache/velt` on Linux, `~/Library/Caches/velt` on macOS,
   `%LOCALAPPDATA%\velt` on Windows), with `$VELT_CACHE_DIR` overriding either?
   **Recommendation:** `$VELT_HOME/cache/std`: one home that `velt doctor` already checks, and
   one place to delete. CI caches one directory.
3. **Loop inlining across the boundary.** Should a mid-sized object function called from a user's
   loop (cost up to `LOOP_COST`) keep its body for inlining, as an `available_externally` copy?
   It costs optimizer time in every program that reaches it. **Recommendation:** not at first;
   mark fast paths where `bench/` shows a loss over 3%, and revisit if the marks multiply.
4. **One object build or one per module, lazily?** Building all of std at once is simplest, but
   the first build on a new compiler (every rebuild in a source checkout) pays for all of std,
   and an edit to one std file rebuilds everything. **Recommendation:** one member per module,
   all built in one fill as proposed, measured; if a fill takes over ten seconds on an M4, split
   fills per module (each keyed by its own and its imports' content).
5. **Fingerprint over VIR or over HIR?** VIR needs std lowered in each program; a HIR fingerprint
   (the signature with pass modes and error sets, plus the layout facts of the types touched)
   would skip that, but it must list every whole-program fact, which is what makes it fragile.
   **Recommendation:** VIR, unless the first pull request measures lowering std above 10% of
   a build.
6. **The `--cpu` in the key.** Keyed by CPU, as proposed, so `--cpu native` builds its own object;
   or build the object always for the default CPU and use it whenever the program's CPU is at
   least that? **Recommendation:** key by CPU. Std's SIMD paths are gated at run time
   (`__intrinsic_target`) already, so a native object gains little, but a wrong one crashes,
   and the key costs only a fill.
7. **The marker's name and form.** `__intrinsic_inline()` as a statement now, or wait for
   decorators (#1664) and write `@inline`? **Recommendation:** the intrinsic now, since only the
   number formatter needs it at first, and a decorator later replaces it mechanically.
