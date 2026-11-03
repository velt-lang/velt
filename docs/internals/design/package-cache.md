# Design: compiling each package once (a per-package build cache)

Status: draft for review (#2189, extends [std-object.md](std-object.md), #1728). Storage follows
the maintainer decision on #1612: `velt_build_cache` owns every compiled artifact on a machine;
this note adds kinds and keys, never a second root.

## Problem

Every build checks, lowers, optimizes and compiles std, and every package it imports, again.
What the build cache reuses today:

| Stage | Reused between builds | By |
|---|---|---|
| Parse of std files | yes | `std-ast` entries (`velt_loader::std_ast`) |
| Collect, sema of std and packages | no | |
| Generators | the offer (which instances, which code) | `generate/cache.rs` |
| Lowering of std and packages | no | |
| velt_opt + clang, std functions (release, `VELT_STD_OBJECT=1`) | per function, by its lowered code | `stdfn` file entries (#1728 step 3) |
| clang, whole codegen units (release) | per unit, by its IR text | `llvm` entries (#1824) |
| Cranelift, per function (debug) | per function | `cl` entries (#1824) |
| Link | the link stamp, when nothing changed | `driver/stamp.rs` |

So the second build of a program, or a build after an edit of the app, still pays for the
front end and lowering of all the std code it reaches, and in release builds for the codegen
unit the edited code shares with std code the std cache does not serve.

### Measured (#2189, release builds, x86-64 Linux, instructions of `velt` and every child)

Main `27f64c7c6`, cachegrind with `--trace-children=yes`, a fresh `VELT_HOME` and build cache per
group of rows. *Again* is the same program rebuilt, *edit* the program with one line added to
`main`, both with the cache the build before filled; `VELT_BUILD_STAMP=off`, so every build
compiles.

| program | off | cache, cold | cache, edit | std cache, cold | std cache, again | std cache, edit |
|---|---:|---:|---:|---:|---:|---:|
| hello | 581M | 595M | 541M | 597M | 496M | 542M |
| `bench/json/parse_typed.vlt` | 9,320M | 10,176M | 7,196M | 10,459M | 1,123M | 3,917M |
| scale benchmark, 10k lines | 89,238M | 91,733M | 91,721M | 91,782M | 16,304M | 91,782M |

- hello, warm and edited: `velt` 307M (parse and sema of the prelude, 26k lines, nearly all of
  it), `ld.lld` 156M (the static runtime), clang 76M (31M of it `clang --version`, run by every
  release build to find clang). No std function is large enough to serve. Only a cache of
  *checked* std removes the 307M.
- parse_typed, edited: the std cache halves the build (7.2G to 3.9G), because the program's one
  codegen unit no longer holds std's reader. What is left is clang on std code the cache does
  not serve (statics with identity, 1.9k statements; small functions, 0.9k) and, before
  #2189's first change, on the code the JSON generator wrote for `Item`; and `velt` itself,
  1.0G, of which sema and lowering of std are most.
- The scale program is app code: an edit of `main` changes all five codegen units (the inliners
  fold the `run` functions into `main`), so it costs a cold build with or without the std cache.
  Only caching the app's own modules, and units that stay put, change that row.

## Model

Go's, as the owner put it on #2189: a package is compiled once, and its result is cached with
compact export data, so a build after an edit redoes only the edited package. Rust's `rlib`
shape for what has to stay generic: object code for non-generic functions, checked IR for
generic ones and for bodies callers may inline.

**Units.** A *package* is the unit of caching:
- std, one package per std module (`std/<module>/`, as `std-layout.md` defines a module);
- each vpm package version (`vpm::PackageGraph` node);
- the program's own package is not cached as a whole; its modules are later work (below).

## The key

A package entry's key (`velt_build_cache::KeyHasher`, BLAKE3) covers everything its results
depend on:

| Part | Value |
|---|---|
| Compiler | `build_cache::compiler_identity()` (version line, executable size and time) |
| Package contents | std: the module's files' contents, already read into the source map (hashing bytes in memory, as `std-ast` keys do); a vpm package: its lockfile integrity hash plus the content hash of its unpacked files when they are a path or git dependency |
| Interfaces of its imports | the interface hash (below) of every package it imports, in import order |
| Target | normalized triple, CPU and its feature mask (`Backend::required_features`) |
| Options that change checking or code | release or debug, `cell_checks`, `debug_info`, the backend and `VELT_LLVM_OPT` (object layer only) |
| Conditions | the build's conditions selecting `#name` variants (`vpm::imports`), and the root package's `define` constants the package's modules are parsed with (`velt_loader`: non-std modules parse with them) |

The interface hash in the third row is what makes this Go-like: an edit to a package's function
bodies that leaves its interface (signatures, summaries, inline and generic bodies) unchanged
does not change the keys of the packages that import it.

## What a package entry holds

Three layers, each usable without the next:

1. **Interface (export data).** The package's collected declarations and checked signatures:
   types, classes and their layouts' inputs, overloads, `extend` members, hook roles (#2161's
   std index), and the summaries sema computes from bodies that callers need: write effects
   (`write-effects.md`), ownership pass modes, error sets, `holds_promise`, fresh returns. A
   dependent is checked against this without the package's bodies. The interface hash is the
   hash of this layer.
2. **Checked bodies** for what dependents instantiate or inline: generic functions and methods,
   `inline()` fast paths, functions under `SMALL_COST`, and generator inputs. Stored as HIR,
   decoded on demand when reach (`lazy_std::reach`) first needs them, the same demand #2161
   introduces for loading std modules.
3. **Compiled code** for the package's non-generic functions above `SMALL_COST`: one object per
   function, as the `stdfn` entries are today, with the facts trace below in place of the
   lowered code in the key.

### Whole-program facts

Lowering is not package-local today. A function's code depends on facts of the whole program
(#1728's measurements: a one-world std object links only 62%, per-module worlds 41-50%):
which types are counted (`Boxing`), whether the program reads a `shared` value (#1816), union
member tags (ordered by `TyId`), class ids and headers, parameter attributes (`readonly`,
`noalias`), frozen and caller-tracking instances, and the counted-types fixpoint's result.

Today's std cache side-steps this by keying on the lowered code itself, which is exact but
means std is lowered in every build. To skip lowering, an entry records the facts its lowering
consulted:

- Lowering reads whole-program facts through one interface, `Facts` (counted? shared? the tag
  of union member M? class id of C?). While a package function is lowered for the cache, a
  recorder wraps it and keeps every question with its answer: the function's *facts trace*.
- An object entry is stored under `(package key, function, instance)` with its trace. A build
  serves it when every question in the trace gets the same answer in this program. Several
  entries (one per distinct trace) can live under one function, as the std cache already sees
  for 156 of 1,292 std functions with more than one variant.
- Class ids and union tags differ between programs by construction. They move out of the
  code: the cached object refers to them by symbol (`statics with symbols and linkage in
  VIR`, std-object.md step 4), and the program defines the symbols. The same change serves the
  1.8-1.9k statements of std code that reaches statics with identity today.
- Counted types are the hard one: they come out of lowering's fixpoint. #2189's lowering work
  (seed the counted types from HIR facts, lower once) makes them a function of the checked
  program. Until it lands, the trace records the counted set of the types the function touches,
  and the build compares that against the counted set its pass 0 found.

### How lowering asks for a cached package

`LowerOptions` gains one optional field, `cache: Option<&dyn PackageCode>`:

```text
trait PackageCode {
    /// A cached object for `func` (instance `args`) whose facts trace holds in `facts`.
    fn lookup(&self, func: DefId, args: &[TyId], facts: &dyn Facts) -> Option<Served>;
}
struct Served { symbol: String, sig: (Vec<Ty>, Ty), param_attrs: Vec<ParamAttrs>,
                calls: Vec<(DefId, Vec<TyId>)>, statics: Vec<String>, object: PathBuf }
```

- Lowering's worklist, before it lowers a package function's body, asks `lookup`. A hit makes
  the function a declaration (`Linkage::Import`) with the cached signature and attributes, and
  queues `calls` (what the object calls without compiling: they must exist and be exported) as
  if the body had called them. The body is never lowered, optimized or compiled.
- A miss lowers the body as today, with the recorder on, and the driver compiles and stores it
  after the build, as `std_cache::link` stores misses today.
- veltc implements `PackageCode` over `velt_build_cache`; velt_vir stays free of storage, and
  unit tests hand lowering a fake.

## Invalidation

Nothing is ever stale: every entry is found by a key that covers its inputs, and garbage
collection removes what goes unused (`velt_build_cache`, least recently used, under the size
limit).
- A new or rebuilt compiler: every key changes (compiler identity).
- An edit to a std file in a source checkout: that module's key changes; the modules importing
  it change only if its interface hash changed.
- A package upgrade: a new version is a new key.
- A program-wide fact that changes (the program starts reading a `shared` value): the entries
  whose trace asked about it miss, and the build lowers those functions.

## Order of work

1. **Measure and fix the std cache's warm path** (#2189's first change): serve the code std's
   generators write; BLAKE3 keys (parse_typed edited: 3,285M; the scale program rebuilt:
   15,086M with the cache alone).
2. **Statics with symbols in VIR** (std-object.md step 4): serves the code reaching vtables, type
   descriptors and module state, and is the mechanism class ids and union tags need.
3. **Facts trace and `PackageCode`**: skip lowering for served std functions. Measured by
   `velt build` of parse_typed after an edit: lowering std drops out of the profile.
4. **Interface layer for std** (export data, sema): skip checking std bodies, with #2161's
   demand loading. Measured by hello's 307M `velt` instructions.
5. **vpm packages** through the same entries.
6. **Debug builds:** the same entries with Cranelift objects (`cl` keys today are per function
   already; the facts trace lets them skip lowering too).
7. **The program's own modules,** per module, once the above holds.

Not part of this plan, found on the way:
- `clang --version` runs on every release build (31M instructions, 6% of a warm hello): its
  answer can be cached by clang's path, size and time.
- Linking the static runtime costs 156M instructions in every release build of hello (`ld.lld`
  over `libvelt_rt.a`, 72 MB).

## Open questions

1. **The interface format:** HIR has no serialized form. A versioned binary encoding like
   `velt_syntax`'s `encode_module` (with its `AST_FORMAT`) is the obvious shape; its size and
   decode cost decide whether layer 1 beats checking std (the prelude's sema is about 300M
   instructions).
2. **Package granularity in std:** one entry per std module, or one per prelude plus one per
   other module? Per module keeps an edit local; the prelude is loaded by every program, so
   decoding one entry for it may be cheaper. Measure both with layer 1.
3. **Facts traces that never repeat:** a function whose trace asks about many program types (an
   `any` glue user) may have a new trace in every program. Such functions should stay in the
   program; a per-function miss counter in the cache can tell.
