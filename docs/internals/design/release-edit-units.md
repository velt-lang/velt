# Release codegen units that survive an edit

Status: proposed. Issue: [#3705](https://github.com/velt-lang/velt/issues/3705).

## Summary

A release build splits a large program into LLVM codegen units and takes each unit's object from
the build cache when its IR text is unchanged. The split gives the entry file a unit of its own so
that an edit there compiles a small unit. The other units change too, though: on
`examples/apps/todo-api` a one-line edit of `main` compiles all five units again, 2.4 million
lines of IR and over a minute of clang CPU. The library units' IR depends on the program's own
code in seven ways, listed below.

This note proposes making each library unit a function of the library code alone and giving the
program's own modules units of their own:
- an inlining allowance for the library and another for the program's own code;
- declarations per unit;
- placement by origin, with a glue unit for what each program generates.

In a prototype, the edit of `main` compiles 17,000 lines of IR (codegen 0.9 s instead of 17 s),
and an edit of a module in `src/` compiles 136,000 (2.3 s). Run time stays within +1.12%
instructions on every benchmark of the nightly set.

## Motivation

### How a release build makes units

`crates/velt_codegen_llvm/src/units/mod.rs` decides:
- **The count.** A program below 64,000 VIR statements (after `velt_opt`) is one unit; above,
  one unit per 16,000 statements up to four, and past 800,000 units of at most 200,000 statements
  (`count_for`).
- **The entry file's unit.** With a count chosen from the size, `plan_apart` places the library's
  functions as if the entry file's were not there, and puts the entry file's functions in one more
  unit, the last. `own_code` recognizes them by their symbols (`_V4main`); every other module's
  functions, the program's own modules included, are library functions there.
- **The cache key.** `clang::compile_cached` keys each unit's object by the IR text and clang's
  identity and flags (build cache kind `llvm`).

### What an edit compiles

The numbers in this note come from one Linux x86-64 virtual machine (4 cores, 15 GB, clang 18.1.3),
the compiler at `main` in release mode, `VELT_BUILD_STAMP=off`, `--release --timings`, and one run
each on an otherwise idle machine; rows measured twice agree within 5%. clang's time is close to
linear in the IR's size, so each table also gives the lines of IR compiled, which do not depend on
the machine. A `VELT_CLANG` wrapper kept each unit's IR, and the causes below come from diffing two
builds' units function by function.

`examples/apps/todo-api` (`demo.velt`) after a clean build:

| Unit | IR lines | clang | Function bodies by origin (lines) |
|---|---:|---:|---|
| 1 | 628,254 | 16.2 s | std 374k, `src/` 57k, http-server 55k, sqlite 21k, glue 82k |
| 2 | 619,115 | 14.7 s | http-server 282k, std 213k, `src/` 25k, sqlite 19k, glue 69k |
| 3 | 594,323 | 16.0 s | std 525k, glue 62k |
| 4 | 593,035 | 16.4 s | std 548k, glue 24k |
| 5, the entry file | 12,122 | 0.18 s | its functions 11,060, imported std bodies 377 |

The entry file's unit is small: std code inlined into its functions, by `velt_opt` or as
`available_externally` copies, costs at most 0.2 s of clang. An edit build is slow because the four
library units miss the cache:

| todo-api edit | Units compiled | IR lines | clang CPU | Codegen | Build |
|---|---:|---:|---:|---:|---:|
| none | 0 of 5 | 0 | 0 | 0.54 s | 9.2 s |
| a string literal in `main`, same length | 1 | 12,122 | 0.2 s | 0.97 s | 10.6 s |
| `console.log` at the end of `main` | 4 | 1,853,563 | 47 s | 17.0 s | 24.5 s |
| `console.log` at the start of `main` | 5 | 2,446,598 | 62 s | 16.5 s | 23.7 s |
| `console.log` in a function of `src/api.velt` | 4 | 2,434,129 | 64 s | 17.2 s | 24.7 s |
| a second module-level `let` in `demo.velt` | 5 | 2,447,999 | | | |

The last row comes from a run on a busy machine, so it gives only units and lines.

The same edits of the other programs:
- **`examples/http_hello.velt`:** 5 units, 1.24 million lines. A `console.log` at the start or
  at the end of `main` compiles all 5 units: 33 s of clang CPU, 10-11 s of codegen.
- **The 10k-line yardstick** (`bench/compile/scale/gen.py 10000`): 5 units, 499,000 lines. A
  `console.log` at the start of `main`, or in a function of `m5.velt`, compiles all 5 (11 s of
  CPU, 3.6 s of codegen); one at the end of `main` compiles 2.

### Why the library units change

1. **The inliner's allowance is the program's.** `velt_opt`'s inliner
   (`crates/velt_opt/src/inline/mod.rs`, `Budget`) lets the program grow by its own size plus
   2,000 statements. Every caller draws on that one allowance, and the program's own functions get
   their share set aside first. An edit that changes the size of the program's own code, or what
   its loops want, changes what is left for library callers, so library functions are inlined
   differently. The called-once rule counts call sites in the whole program: a library function
   the program starts calling is no longer inlined into its only library caller. The `console.log`
   at the end of `main` changes 32 library function bodies in three library units (http-server's
   HTTP/2 connection and std's HPACK encoder among them).
2. **Instances per call site live in library units.** A small std function that reports its
   caller's location gets an instance per call site (`crates/velt_vir/src/lower/track_caller.rs`),
   named after the call site (`…statAt_L1_21_7`) and holding the location's text
   (`" at demo.velt:21:5"`). Placement counts the instance as std's, so a line inserted above the
   call renames it and changes a library unit.
3. **Every unit declares the whole program's externs.** `emit_unit_capped`
   (`crates/velt_codegen_llvm/src/module.rs`) writes the declarations of all of the program's
   externs and thread-local globals, in the program's order, into every unit. An extern the edit
   adds or moves changes every unit's text.
4. **The program's other modules are library code.** `own_code` cannot tell `src/api.velt`'s
   symbols from a package's, so todo-api's `src/` code (82,000 lines of IR) is placed in two
   library units of 600,000 lines, and every edit of it compiles them.
5. **Glue for the program lives in library units.** `velt_main`, the module-state initializer
   (`_Gmodule_init`, `_Gmodule_ready`, the descriptor) and glue for the program's types
   (`_Gdrop_…m1::Item10[]` in the yardstick) are placed with the libraries and draw on their
   allowance (cause 1). A module-level `let` changes the descriptor, the initializer and the drop
   glue of the state tuple, all in a library unit, and their size changes the library's inlining.
6. **Module state is numbered across the program.** An access of a library module's state names
   its slot number and the program's descriptor
   ([hir_encodings.md](../contracts/hir_encodings.md#module-state)), so a slot added before it
   renumbers it. std's `navigator`, `storage` and `this_fn` modules have module state; todo-api
   reaches none of them (its only state is the entry file's), so cause 5 is what its `let`
   measures.
7. **Newly reached library code.** An edit that calls a library function no code called before
   adds that function to a library unit. todo-api's first `console.log` in program order reaches
   `process.firstWrite`, and the 490,000-line unit it joins is compiled again.

## Design

An **app function** is one of the program's own modules: the entry file and the other modules of
the root package, which `velt_loader` loads from the program's directory. An instance made for a
call site in an app function is an app function too. Every other function is a **library
function**: std's, a package's, or glue. The rule: **a library unit's IR depends only on the
library functions and their inputs**, never on the app functions' code.

### Declarations per unit

A unit declares only the externs and thread-local globals that its bodies and statics name, in
symbol order (`velt_codegen_llvm` `module.rs`). This costs nothing at run time.

### Two inlining allowances

The inliner (`velt_opt` `inline`) keeps two allowances:
- **Library callers** draw on the library's own size plus 2,000 statements, first come, first
  served, as library callers share it among themselves in `Budget`.
- **App callers** draw on the whole program's size plus 2,000, the allowance the whole program has
  without the split, so the app's hot loops lose nothing. Shares are set aside among app callers
  by `wants` and `Budget::share`.
- **The called-once rule** counts, for a library caller, only the library's call sites. A
  function called once in the library and also by the app is inlined into its library caller and
  kept for the app's calls.

The program may grow by at most its size plus the library's size, plus 4,000 statements. As far
as the inliner decides, a library function's body after `velt_opt` depends on the library only.

### Origin in VIR

Lowering records each function's origin (app module, library, glue for the program) and its
module's key on `vir::Function`. `units` reads it instead of parsing symbols (`own_code`,
`is_root_module`). This is a change to the `vir.rs` contract.

### Placement

- **Library units:** placed from the library functions alone, as `owners_apart` places them.
  Their count is chosen from the library's size, so the app's growth does not change it.
- **One glue unit** for what each program generates: `velt_main`, the module-state functions and
  descriptor, and glue (drop, clone, closure environments, vtables) of the app's types.
- **App units:** one per app module, in module-key order. A call-site instance goes with its
  caller's module. App units take `available_externally` copies of small library and app bodies
  (`assemble`'s imports). A library unit takes no copy of an app or glue body.

`velt_opt` still sees the whole program, and app functions still inline library code, which keeps
run time where it is. Only the library's inlining stops seeing the app.

### Module state per module

Each module's state is a block of its own, reached by symbol, as in
[build-architecture.md](build-architecture.md). A slot added in one module then renumbers nothing
in another (cause 6). The glue unit above already takes the program's descriptor and initializer
out of the library units (cause 5).

### What an edit compiles

| The edit changes | Units compiled |
|---|---|
| bodies of an app module's functions | its unit, and app units that copied a changed small body |
| the program's types or module state | the glue unit, and the app units that use them |
| what library code is reached (cause 7) | the library units the new functions join |
| a library type's instance over an app type | the library unit holding the instance |
| std, a package, or the compiler | every unit whose code changes |

## Runtime cost

### Edit builds

A prototype built the design with four simplifications, switched on by environment variables in a
local build:
- origin from symbol prefixes, not a VIR field;
- no glue unit: glue stays with the libraries;
- the module-state change left out;
- declarations sorted, but still all of the program's.

| Program, edit | Base: units, IR lines, codegen | Prototype |
|---|---|---|
| todo-api, clean build | 5 units, 2.45M, 17.0 s | 10 units, 2.54M, 17.9 s |
| todo-api, end of `main` | 4, 1.85M, 17.0 s | 1, 16.9k, 0.90 s |
| todo-api, start of `main` | 5, 2.45M, 16.5 s | 2, 507k, 11.9 s |
| todo-api, `src/api.velt` | 4, 2.43M, 17.2 s | 2, 136k, 2.3 s |
| http_hello, clean build | 5, 1.24M, 9.6 s | 5, 1.24M, 9.9 s |
| http_hello, end of `main` | 5, 1.23M, 10.3 s | 1, 12.1k, 0.66 s |
| http_hello, start of `main` | 5, 1.23M, 11.2 s | 2, 222k, 6.9 s |
| yardstick 10k, clean build | 5, 499k, 3.4 s | 18, 583k, 3.4 s |
| yardstick 10k, end of `main` | 2, 150k, 3.0 s | 2, 64k, 1.07 s |
| yardstick 10k, start of `main` | 5, 499k, 3.6 s | 2, 64k, 1.08 s |
| yardstick 10k, `m5.velt` | 5, 499k, 3.6 s | 2, 76k, 1.13 s |

The whole todo-api build after an edit at the end of `main` goes from 24.5 s to 8.2 s, and after
an edit of `src/api.velt` from 24.7 s to 10.5 s. What still misses:
- **todo-api's and http_hello's start-of-`main` rows** compile a library unit because the edit
  reaches `process.firstWrite` (cause 7).
- **The yardstick's rows** compile a library unit that receives the drop glue of an app type
  (cause 5): this is what the glue unit removes.

The second module-level `let` still compiles every unit in the prototype, since it has no glue
unit. The glue unit and module state per module are what fix that row.

### Run time

Origin and placement change only programs split into units. No program of the nightly set is,
so for those programs only the two allowances change code. Each of the 129 programs
(`bench/nightly.sh`'s list) was built by the base compiler and by the prototype and run under
cachegrind with `VELT_THREADS=1`. Instructions:
- **Slower by more than 0.5%:** 4 programs, the most `json/parse_union` (+1.12%),
  `weakmap_handlers` (+1.07%) and `stdport/brotli_stream_300k` (+0.83%).
- **Faster by more than 0.5%:** 18 programs, among them `stdport/sha512_large` (-16.4%),
  `stdport/sha384_small` (-11.7%), `json/edit` (-6.5%) and `counted_field_args` (-4.1%). The
  allowance binds in these programs, and the app's own allowance no longer competes with the
  library's.
- **The rest:** within ±0.5%.

The executables' `.text` grows 0.25% in total, and 3.0% at most (`async/timers_due_in_promise`).

A program large enough to be split keeps the placement of its library units by call chains. Its
app code moves to units of its own: calls from app code into library code keep their
`available_externally` copies of small callees, and calls between app modules in different units
become calls between units. A split program's run time under this placement is not measured yet;
todo-api's server and the yardstick are the programs to measure it on.

### Clean builds

Codegen of a clean build goes from 17.0 s to 17.9 s on todo-api, from 9.6 s to 9.9 s on
http_hello, and stays at 3.4 s on the yardstick, where more, smaller units compile in parallel.
clang CPU: 63.5 s against 63.1 s on todo-api, 11.1 s against 12.3 s on the yardstick.

### What an edit build is left with

After an edit at the end of todo-api's `main`, the prototype's build is: parse 0.12 s, sema 1.9 s,
lower 1.1 s, `velt_opt` 4.0 s, codegen 0.9 s. `velt_opt` is half of it. With the two
allowances, the inliner treats a library function the same way in every build of the program,
which a cache of optimized library functions needs. Caching lowering is part of
[build-architecture.md](build-architecture.md).

## Alternatives considered

**No cross-unit inlining in edit builds.** Keeping std's code out of the app's unit (an
`--incremental` mode, or inlining across units only in a full release build) shrinks a unit that is
already small: todo-api's entry file unit is 12,000 lines and compiles in 0.2 s. It would cost run
time in the app's loops, which call std, and edit builds would run different code from release
builds.

**A link-time ThinLTO pass, as Rust's split does.** Clang compiles each unit to bitcode with a
summary, and the link imports and optimizes across units. The units already import small
functions as `available_externally` copies, which is the import ThinLTO would add. What the units
lack is stability, and ThinLTO needs `lld` with LTO on every target and an optimization step in
every link.

**One unit for all app code.** On the yardstick the app is 469,000 lines of IR: one unit for it
takes 12 s of clang per edit, and a clean build takes 13.0 s of codegen instead of 3.4 s.

**Every function only app code reaches, in the app's units.** This also takes care of newly
reached library code (cause 7). On todo-api it moves most of http-server and of std's HTTP code
into the app's unit: 1.03 million lines, 27 s per edit.

**One shared allowance with the library's part set aside first.** The library's part would still
be a share of the program's size, so app growth would still change library code.

**Library units placed by module key instead of by call chains** would confine cause 7 to one
smaller unit. Placement by call chains keeps a hot loop and its callees in one unit
(`units/place.rs`; k-nucleotide ran 12% slower with `Map.lookup` in another unit). Whether cause 7
is worth that cost is measured once the rest is in.

## Limitations

- Reaching library code that the program did not use before compiles the library unit the new
  code joins.
- A change to an app type's layout changes library instances over that type, in library units.
- App units copy small bodies from each other, so an edit of a small app function also compiles
  the units of its callers in other modules.
- Parsing, checking, lowering and `velt_opt` still process the whole program at every build.
