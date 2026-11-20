# Compiling std's Velt code once

Status: partly implemented. Issue: [#1728](https://github.com/velt-lang/velt/issues/1728).

## Summary

Standard library code written in Velt would otherwise be compiled again inside every program that
uses it. The **std cache** compiles std functions once, optimized by LLVM, keyed by their
canonical code, and stores the objects in the build cache; a program whose lowered std function
has the same key links the object instead of compiling the function. Release builds use it for
the functions std marks `@noInline`, debug builds for every function the cache holds, so debug
programs run std's code optimized. **Prebuilt hook modules** go further for std modules that
replace runtime code: their objects are linked without the module being loaded or checked at all.

## Motivation

Code in the Rust runtime is compiled once, when Velt is built, and only linked
([Linking](../linking.md)). Each move of std code from Rust to Velt (the JSON engine, number
formatting, the scheduler) adds compile time to every program that reaches it: a release build
that prints a non-integer number compiles the float formatter, and a program that uses the HTML
escaper compiles it, in every build. Debug builds also get std's code unoptimized, compiled by
Cranelift without optimization, which makes a std port much slower in debug than the Rust it
replaces.

## Design

### Which builds use the cache

`crates/veltc/src/driver/std_cache/mod.rs` (`wanted`) decides:

| Build | Std cache |
|---|---|
| Release, LLVM, no `-g` | The functions std marks `@noInline` (`no_inline.rs`) |
| Release with `VELT_STD_OBJECT=1` | Every function the cache serves, compiling and storing misses |
| Debug (either backend, with or without `-g`) | Every function the cache holds (`serve.rs`); a miss stays in the program |
| `--emit obj`, `--emit vir`, `--emit llvm`, `--report numbers`, `--host` | Off: the output must be self-contained, or reads std's code |
| WebAssembly | Off (no objects to link); a release build with `VELT_STD_OBJECT=1` uses it |
| Debug builds for arm64 Linux | No prebuilt hook objects: LLVM's atomics there call the outline-atomics helpers (`__aarch64_*`), which a debug link need not have (the shared runtime does not export them; the bundled linker has no libgcc archive) |
| `velt check`, `velt fmt`, `velt lsp`, `velt doc` | Never build or read objects (they stop before code generation); `velt check` looks up prebuilt hook objects as a debug build would |

`VELT_STD_OBJECT=0` compiles all of std in the program everywhere, for debugging std itself and
for A/B measurements. `VELT_STD_OBJECT_STATS=1` prints what the cache did and why std code stayed
in the program.

### The std cache

#### Keys

After lowering, every std function the cache may serve gets a key
(`crates/veltc/src/driver/std_cache/key.rs`): its canonical code (`velt_vir::fingerprint`,
`Mode::Link`) and parameter attributes, those of every function compiled into its object, and who
compiles it for what (`build_cache::compiler_identity`, clang, target, CPU, optimization level,
and whether a release or a debug build links it). Std's code includes what std's source
generators write for a program (`JSON.parse<T>`'s reader of `T`), served like a generic instance.

The link-mode fingerprint names functions and externs by symbol, statics by their whole
relocation graph and aggregates by their whole structure (async state aggregates included), and
includes the throw location an `Uncaught …` report records, each extern's declaration and the
function's gated target. Source locations and debug variables are left out; panic texts are
statics and so are included. Std paths in texts compiled into programs are relative to the std
root (`crates/velt_vir/src/lower/srcloc.rs`), so a key is the same in every checkout and install.

Code with a key is the same in every program that makes that key, so whole-program facts (class
ids, layouts, counted types) that show in the code are covered by the key. **The parameter
attributes that depend on the whole program** do not show in the code: `noalias`, `readonly` and
`frozen` (`crates/velt_vir/src/lower/param_attrs.rs`) differ, for example, in a program that sorts
an array of a class holding a string with a comparator, where `string` parameters lose
`readonly frozen`. A release build therefore keys each object two ways (`key::Assumed`): with the
program's attributes, and without those three for an object whose program of its own used none of
them (`compile::program_of`). Leaving an attribute out only takes a fact away from the optimizers,
so that object is right in every program with the same code and the same other attributes. A
build links the first key if the cache has it, else the second; an object compiled with an
attribute is never linked into a program without it. Debug builds key with the program's
attributes only.

`--cpu native` is keyed as the word, not the host's CPU, as the LLVM unit cache keys
`-march=native`, so a cache shared between machines can link objects another machine built for
its `native`. Debug objects are always keyed for the target's default CPU, so `velt run` and
`velt test` (which build for `native`) link the same objects as `velt build`; a program for a CPU
lacking what the default requires links none.

#### What is served

`crates/veltc/src/driver/std_cache/select.rs` decides. Every function defined in std may be
served, generic, caller-tracking and frozen instances included, except:

- in a release build, **small functions** (under 40 statements, velt_opt's `SMALL_COST`) and those
  marked `@inline`, which stay where callers can inline them; they are compiled into the cached
  functions that call them and so are in those functions' keys. A debug build inlines only the
  helpers `velt_opt::debug_inlined` names and keeps only those;
- in a release build by default, every function std does not mark `@noInline`: the optimizer may
  inline or specialize any other into its callers, so only a function it never inlines runs the
  same from an object;
- **renamed duplicates** (`$dup`), which are not the same function in every program;
- functions whose code reaches a **static with identity that cannot be named** (below);
- in a release build, functions that **call through a function pointer**: in the program
  `const_fields` specializes them for the closure their caller passes and inlines it, which an
  object compiled once cannot;
- in a debug build, code that shows **a place in the program's own files** (a caller-tracking
  instance per call site): its key holds that absolute path, so no other program could link it;
- in a release build by default, functions the optimizer would **clone for a function gated on a
  CPU extension** (`gated_clones`), and gated functions whose object would call one: in the
  program the clone has the extension check folded away;
- **never served**: in a release build, the functions `select::NEVER_SERVED` names. It is a
  tool for std's authors, beside `@noInline`: a `@noInline` function whose callers, inlined into
  the program, pass it constants LLVM specializes it for (`fixedText`, which `toFixed(2)` calls
  with a constant `digits`). Each entry says why, and a test fails on a name std no longer
  defines. Constants std itself passes are better fixed in std, with an `@inline` body under
  `@noInline` entry points.

#### Identity of statics

A static with relocations (vtables, type descriptors, `any` tables) or
one the program writes (module state, static atomics) is compared or written by address, so an
object cannot carry a copy. Such a static, and what its relocations reach, is named instead
(`crates/veltc/src/driver/std_cache/statics.rs`, vir.rs invariant 13): `_Vs<content
fingerprint>`, the same in every program with the same static. The program defines it (`Export`,
exporting the functions its relocations name), and the function's own program declares it
(`Import`, with its contents, so the code is optimized as it would be in the program). Only two
statics of one program with the same contents cannot be named.

### Release builds: the `@noInline` functions

The optimizer never inlines a function std marks `@noInline`, so its object holds the code the
program would have compiled for it, and the program's clang no longer compiles it: the HTML
escaper's entry points, for example. Every other std function stays in the program, where it can
be inlined and specialized. Such a build:

- looks in the cache only when the cache has held a std function entry and the program has a std
  function marked `@noInline`; otherwise it does nothing a build without the cache would not do;
- links what it finds and leaves a miss in the program, starting no clang for it;
- compiles each object with copies of its own of every function the served function reaches
  through functions not marked `@noInline`, whatever their size, all in its key
  (`select::Depth::Whole`), because velt_opt and LLVM inline large callees into it in the program
  too. A function whose address is taken stays the program's, with one address, and so do the
  `@noInline` functions it calls;
- takes a served function out of the program after velt_opt has optimized it, not before
  (`Served::take_out`). While velt_opt runs, the served function occupies the inliner's
  program-wide budget (`Budget::for_program`) as it would without the cache, so the rest of the
  program is inlined the same way. A clone velt_opt makes of a served function (`const_fields`,
  `null_exits`) is the program's own;
- has velt_opt leave a served function's body to the inliner (`velt_opt::optimize_served`): the
  first per-function pass works on it before inlining, and the inliner inlines its callees into
  it, so its calls count and keep alive what they call, but no later per-function pass
  (`constfold`, `sroa`, `dce`, `bounds`, …) spends time on a body that is then dropped. The
  release VIR of the end-to-end tests and the std benchmarks is the same with this skip as
  without it, apart from what is served.

`velt cache warm` builds its warm-up programs in release too, which stores these objects.

### Debug builds

A debug build (`std_cache/serve.rs`):

- links what it finds. A miss stays in the program and Cranelift compiles it with the rest, so a
  build costs the same or less. A debug build never starts clang, which may not be installed, and
  stores nothing: `velt cache warm` stores objects (`VELT_STD_OBJECT_FILL`, with clang present);
- serves more than a release build: small functions and those marked `@inline` too, and functions
  that call through a pointer, since a debug build specializes no closures. An object is compiled
  with the small functions it calls (`select::Depth::Small`);
- places each object before the function comes out of the program, so an object that has gone
  from the cache since is one more miss and no build starts over;
- shares the program's thread-local values with the objects: Cranelift's `velt_tls_data_<id>` are
  `hidden`, as LLVM's are. Where Cranelift keeps them on thread keys instead (arm64 Windows), code
  reaching one stays in the program;
- works out what the program reaches outside itself (`outside::reaches`) before functions become
  declarations, which would hide it. A source generator host and the `velt dev` JIT host, which
  has no link, stay whole.

With `-g`, a debugger steps over std as it steps over the runtime: the objects have no line
information. A stack trace names the functions it finds by symbol; a helper inlined into an
object has no frame of its own.

### Prebuilt hook modules

The objects above replace code generation only: sema, lowering and velt_opt of a served function
still run in every build. For std code that replaces runtime functions, the front end is most of
the cost. So an on-demand hook module whose `prebuilt` hooks (`crates/velt_common/src/std_hooks/
prebuilt.rs`) take and return only numbers, `bool`s and strings is compiled once, with everything
it reaches, into one object (`std_cache/prebuilt.rs`, `prebuilt_object.rs`):

- **Front end skipped.** When a check meets a `hookCall` of such a hook and the cache holds its
  module's object, sema types the call against a declared function `velt_hook_<entry>`
  (`crates/velt_sema/src/body/lazy_std/hook_stubs.rs` `declared`), as it would a `velt_rt_*`
  extern; the module is never loaded, checked, lowered or optimized, and the link adds the object.
  A `lowered` hook its constructs demand is declared the same way (`declared_lowered`), and
  lowering's builder calls the declared function as an extern (`crates/velt_vir/src/lower/
  hook_calls.rs`). Without the object the build loads the module as for any hook, so no output
  depends on the cache.
- **No program facts.** Numbers and `bool`s cross by value (a `bool` as both backends pass it, a
  zero-extended byte); a `&string` as the address of the caller's string, borrowed; a `string`
  result through a trailing out-pointer the callee fills (the runtime string externs'
  convention). Nothing a program decides reaches the object's code, which is compiled in a world
  of its own: a root calling each hook once, everything internal but the entry points. Its key
  names the compiler, LLVM's optimization flag, the target, the CPU (the default one for debug
  builds), the std root and the module; a manifest under it lists the std files the world loaded
  (hash, size, time), which a build checks and which go into the program's build stamp. Refused:
  an entry not lowered as the convention says, and code sharing state with the program (module
  state, thread-local values, static atomics, named statics) outside the module's private
  modules.
- **State of its own.** A module may keep thread-local values and static atomics in its *private*
  modules (`prebuilt::private`: modules nothing else imports, such as the weak core's
  `weak*.velt`, and module_blocks.velt, whose blocks of module state are refused in every other
  module's object). The object keeps the static atomics; the manifest lists the thread-local values'
  externs, and a build linking the object adds them to its program, whose unit defining
  `velt_main` defines their storage (vir.rs invariant 5: code compiled to link into a program
  declares them). Sema calls no object for a program that loads a private module itself, which
  would hold a second copy of the state; and where a debug build's backend has no thread-local
  storage an object can share, an object with thread-local values is a miss. An entry point must
  not take its caller's location (`track_caller.rs`), so such a module's code has no panic but its
  internal errors'.
- **Pinned.** Once sema has called the entry points the build must link the object, so a build
  hard-links (or copies) the object it finds into a temporary directory it holds until it is done;
  another build's garbage collection or `velt clean --cache` cannot fail it. An object that
  cannot be pinned is a miss.
- **Filled** by `velt cache warm`, which compiles every prebuilt module's object for the default
  CPU and for `native`, and by `VELT_PREBUILT_HOOKS=fill` after a build that missed.
  `VELT_PREBUILT_HOOKS=0` turns prebuilt objects off. WebAssembly loads the module.
- **Carried by the toolchain.** A cached object's key names the machine (the compiler
  executable's size and time, the absolute std root, each file's size and time), so it cannot be
  handed to another one, and a build before the warm-up, without clang, in CI or with the cache
  off compiled the module into the program. A release therefore carries the objects for each
  target's default CPU in `lib/hooks/<target>` (`std_cache/shipped.rs`), made when it is packaged
  (`velt cache hooks`, from `scripts/package.*`). Their manifest names nothing of a machine: the
  compiler's version line, the target, and each std file as its content hash and its path below
  the std root. An installed `velt` that finds no object in the cache takes the toolchain's when
  the version line is its own and every listed file hashes as recorded, which is what keeps it
  from linking an object against std sources that were edited. With the cache on it then stores
  the object under its own key, and later builds find it by sizes and times as any other; with
  the cache off every build makes the hash check. A `--cpu native` release build still needs the
  cache's own object.

## Compilation

**VIR.** A served function becomes a `Function` with `Linkage::Import` (vir.rs invariant 11):
defined elsewhere, never emitted, treated by passes like `Export`. A pass may change a function's
signature or parameter attributes, or assume facts about its callers, only for an `Internal`
function. Before optimization, each served function becomes an `Import` and the program's
functions its object may call become `Export`, so velt_opt takes no facts about them from the
program's calls. After optimization the program links the objects of the declarations it still
calls (the optimizer drops a declaration nothing calls, as it would have dropped the function),
closed over what those objects call.

**Objects.** A miss under `VELT_STD_OBJECT=1` is compiled on its own, with unknown callers, in a
program of its own (itself, the functions it may inline, declarations of what they call, the
statics they name), and stored in the build cache (`velt_build_cache`, kind `stdfn`, a file
entry). If a function cannot be compiled on its own, the build optimizes the program as it was,
without the cache: the cache costs speed, never correctness. A build compiles its misses only when
they come to 1,000 statements or more, because each costs a clang process; below that they stay in
the program this time, and a larger build or the warm-up stores them.

**Linking.** The build stamp records the objects a build linked, and the fast path links them
again. The link reads them through hard links (or copies) named like object files,
`<program>.std/<n>.o`, so MSVC's linker, which reads a name without an extension as `<name>.obj`,
takes them too, and a build keeps linking what it was built with whatever the cache's garbage
collection does. Builds of one output take turns on `<program>.std.lock`
(`std_cache/turn.rs`), since each replaces that directory; a build that has to wait says so. On
Windows a debug link passes the objects through a response file once their paths no longer fit a
command line.

**Calling convention.** Both backends give every VIR function the target's C convention on
scalar-only signatures, so a Cranelift debug program calls an LLVM-built object directly.

## Runtime cost

A served function is compiled without its callers' facts: its object cannot see constant
arguments a program would fold into it, which is why `NEVER_SERVED` exists, and in a debug build
std's code runs optimized instead of unoptimized. Release builds serve only functions the
optimizer never inlines, so the program's own code is unchanged. At compile time, a hit removes
the function's code generation; a debug miss costs nothing extra, and a release build starts no
clang for a miss.

## Alternatives considered

**One object for all of std.** Building every non-generic std function into one object, in a
world that loads all of std, links only a fraction of the std code programs compile: whole-program
facts, above all which types are counted, differ between that world and a program, and adopting
the world's facts would box `f64[]` and `u8[]` in every program. Fixing class ids and headers in
tables for every program would only partly align them. Cached one by one, keyed by their code,
std functions serve most of it, and what a program links is exactly what it would have compiled.

**Incremental compilation of all code.** Caching every function's object under a hash of its VIR,
for user code too, is more general, but each function becomes its own object and the cache churns
with every edit. Std is the part that does not change between builds.

**ThinLTO summaries or LLVM bitcode.** Only the LLVM backend could use them, and each link runs
LLVM's optimizer on the imported code: the code-generation time comes back at link time.

**Keeping a Rust copy.** Compiling std's hot parts in Rust means calls the program cannot inline
and a second implementation to keep in step.

## Limitations

- Cached objects carry no line information, so release builds with `-g` do not use the cache.
- `velt dev`'s JIT host compiles std in the program: it resolves symbols from a table and cannot
  load an object file.

## Not yet implemented

- **Prebuilt objects in the toolchain.** Release packages and target packs carry std objects for
  the default CPU, as .NET's ReadyToRun images do, so a first build links optimized std code
  without `velt cache warm` and without clang.
- **The JIT host keeping std compiled** across reloads.
