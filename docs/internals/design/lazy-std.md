# Standard library checked on demand

Status: implemented (#1658; loading during the check, #2161 step 3). Owner: `velt_sema`
(`body/lazy_std/`, `std_load.rs`).

## Problem

The checker used to check every function body and module constant of every loaded module.
The prelude imports std modules that most programs never use (`velt/lang/number/parse` and its
1,300-line table of powers of five, `velt/encoding/json/reader`, `velt/time/*`), so hello world
paid to check all of them. Lowering builds only what `main` reaches, so none of that code ends
up in the program. Each such module made every program slower to compile: about 3% for
`velt/lang/number/parse`, about 20% more for `velt/encoding/json/reader`.

## Design

A std module is *lazy* unless:

- a user module imports it directly, or it is a user module's JSX runtime. `velt check` of a
  module that imports std still reports everything in what it imports (but not the prelude's
  free functions and static methods it doesn't reach, below);
- lowering looks its definitions up by name (`EAGER`: `std/sort`, `std/proxy`,
  `std/typed_array`, `std/sync/epoch`, `std/this_fn`);
- it has top-level statements, which run when the module is evaluated.

The definitions of a lazy module that are *deferred*:

- free functions and static methods: checked once checked code calls them or takes them as a
  value (`Callee::Def`, `FnRef`);
- module `const`s, read with `Global` (not in the prelude);
- the methods and constructors of its classes and structs (not in the prelude): checked once
  checked code *uses the type*, that is once the type, or a type containing it (an array, an
  option, a type argument, a field of a used type, a base class), is the type of a checked
  expression or local or a type argument of a checked call (`probe<Gadget>()`, whose body
  monomorphization builds with `Gadget`'s methods). A value of a class exists only where such
  an expression creates or holds it, so a virtual call or an interface call can only reach the
  methods of a used type. Lowering's glue for a class value (printing, `JSON.stringify`, a
  member of `any`) reaches further: it dispatches over every class in the program that extends
  the value's class (`velt_vir` `any_subclass_dispatch`, `json_descendants`), constructed or
  not. So a used class makes its subclasses used too, transitively: printing an `Error` checks
  the methods of every `Error` subclass the program loads (their `[util.inspect.custom]`, `toJSON`, …).

The methods of an `extend` block on a number type, `bool`, `string` or a `velt/simd` vector in a
lazy module, the prelude's included (`extend u32 { rotateLeft(…) }`, the checked, saturating and
wrapping arithmetic, a vector's lane operations and operators): checked once checked code calls one or takes it as a value (`Callee::Def`, `FnRef`),
as a free function is, or once an interface implementation holds one (`hir::Program::impls`:
`Comparable`'s `compareTo` of every number type, `extend u64 implements InlineKey`, a number
converted to an interface), since lowering calls those through the implementation (a bound's
method, a `dyn` table) without checked code naming them. Every program uses the primitive
types, so waiting for the type to be used, as for a class, would check them all. No class's
`any` table or subclass dispatch lists such a method, and a value of `any` holding a number
reaches none of them (its built-in methods are the `__any<Type>_<method>` helpers below).

The prelude (`std/prelude/`, `std/record`, `std/collections`) is lazy in a narrower way:
lowering looks up its types' methods and its helper functions by name, so only its free
functions and static methods (and the extension methods above) are deferred, except the helpers (`HELPER_NAMES`: every `"__…"`
name in the code of lowering, the optimizer and the backends, which a unit test checks, and the
`__any<Type>_<method>` built-ins of `any` values). Module `let`s and static fields are never
deferred: they are module state.

`body::check_bodies` checks everything else as before. Then `lazy_std::reach` scans the HIR
checked so far (function bodies, constant initializers, field and parameter defaults) for
references and for the types of every expression and local. It checks what they name, scans
what that adds, and repeats until nothing new is reached. Each definition and each type is
scanned once, so the cost is one walk over the checked HIR. A deferred function that a call
names is checked before the call is (`body::returns::ret_of`, which every call asks for the
callee's result): what a caller's check learns from the callee's body (its inferred result,
what it keeps or modifies) is there, as when every body was checked first.

What nothing reached gets a stub (hir_encodings.md "Unchecked definitions") and is listed in
`hir::Program::unchecked`. Lowering ICEs if it ever builds one. That would mean sema missed a
reference, for example one lowering makes by name to a definition outside what stays checked.
Glue that will call a std definition by name must keep it checked: its module goes in `EAGER`,
or, for a prelude function, its name in `HELPER_NAMES`. A unit test scans the code of lowering,
the optimizer and the backends for `"std/<module>::<name>"` literals and checks that each one
stays checked.

The JSON glue was the one exception (`JSON_GLUE`: the modules its decoders and encoders called
by name, reached at the first instantiation of a JSON intrinsic). `JSON.parse`, `JSON.stringify`
and velt/db's row readers and binder are generated code now (source-generators.md), which
reaches what it calls as any checked code does, so the exception is gone with the intrinsics.

So a prelude method that stays checked but that nothing calls (`JsonValue.as<T>`) costs hello
world nothing. What the reader and writer call in other modules (`std/json/scan`,
`std/number/*`) is reached from their code as usual. Eagerly checked std code that encodes a
concrete type (`JSON.stringify` in the typed arrays' and proxies' code) reaches the writer for
every program.

The editor's analysis (`ide::check_for_ide`) checks everything, since hover, go-to-definition and
diagnostics in std files need it. `VELT_CHECK_ALL_STD=1` turns laziness off for the command line
too, to compare.

## Loading on demand

Checking on demand still parses every module a program loads, and parsing and collecting std code
costs about as much as checking it. So some of std is not loaded at all unless a program needs it.

**During the check (#2161 (a1)).** The modules of hooks no program needs by default
(velt_common `std_hooks::ON_DEMAND_MODULES`: `velt/lang/hooks/number_text`, `number_float` and
`any_record`) are not loaded with the prelude. `reach` finds out, from checked code and its types,
that one of their hooks is needed (a live construct's integer hook, `live.rs`; a text conversion in
the program's own code of a value that may hold a float, `number_text.rs`; a conversion to a record
with string keys, `record_hooks_needed`), and demands the module (`Ctx::demand_std`, `std_load.rs`):
- The driver's load callback (`velt_sema::LoadFn`) goes on from the program's load
  (`velt_loader::load_more`) with the module and what it imports that is not loaded. Each must
  follow the rule of `velt_loader::std_index`, so what it adds stays local to it; the loader applies
  the rule to the parse at hand, and reads no index entry. A module that breaks it is an internal
  error. `tests`: `std_index::rule_tests`.
- Sema runs the source rewrites on the batch alone, appends the modules, collects what they declare
  (`collect::collect_added`: every pass of `collect`, from where the additions start) and checks
  their eager bodies, then `reach` goes on. Existing `DefId`s do not change.
- The batches outlive every `Ctx` and every check of the program (`StdLoads`). A later round starts
  from the program's modules and demands again: it takes the kept batches, in the order they were
  served, so a round's `DefId`s are the first round's.

`VELT_EAGER_STD=1`, and any caller of `load_program` that does not ask for `demand_std`, loads all
of these modules up front, after everything else, and gives the check no callback. Every golden's
`--emit vir` is byte-identical both ways (`crates/veltc/tests/eager_std_identity.rs`, selected by
`cargo xtask affected` when sema, the loader, lowering or std change): a difference is a rewrite an
added module missed, or something in the VIR that depends on the order sema numbers things in. VIR
does not name a `TyId` or a `FileId` for that reason (`Cx::view_key`, the `_L` of a
caller-tracking instance's symbol).

The editor runs the same demand set: `ide::check_for_ide` takes the callback, and with it works out
what `reach` would check as if bodies were deferred (`Lazy::check_all`), though it checks them all.
Without the callback, a module the program did not load stays out and its hooks are not checked,
as with a std root that lacks it: `velt check`'s lint rules and `--ts-compat` analyse a program
loaded on demand that way, since they read only the program's own code.
`velt_lsp`'s `tests/demand.rs` requires the errors and the modules of `velt check`. `velt check
--std` and the editor's workspace index load everything, and demand nothing.

A demand must come before the passes that follow the bodies' check (`hooks::check_shapes` and the
others up to `live::finish`), which would not run for what it adds. `live::finish` walks what those
passes make, so a demand from there is an internal error (`Ctx::demand_std`), which no program
reaches yet; a float hook found there is left to the runtime's printer, as in std's own code.

std's integer hooks are checked only where *live* code has a construct whose lowering calls them
(#2161 step 2; velt_common `hook_demand.rs`): a template or `String()` of an integer, the formatting
glue of a type that may print one (`console.log`, a conversion to an interface, whose table holds
that glue), an array index (its out-of-bounds message) or an `any`. A class's own table has its
`format` slot only where the program formats (`Cx::format_demanded`), so a class or struct alone
demands nothing. A function may get a `throws` that needs formatting from what it implements
declares, which is known only after the passes, too late to load a hook: the check then runs once
more with `Format` assumed (`thrown_needs_format`). Live code is a superset of what lowering builds: the program's
own and the std code it reaches (`velt_sema` `body/lazy_std/live.rs`), std's own conversions
included, which a debug compiler or `VELT_LIVE_CHECK=1` has lowering assert, so hello world checks
none of std's integer printing.

`host_async` is not moved: `velt/host/promise` loads `velt/io/error`, whose `IoError` extends the
prelude's `Error`, which the rule excludes. It keeps its trigger below until (a2) (#2161 step 4).

**Before the check.** A global module under `std/prelude/global/` whose header names a condition is
loaded by the loader before any types are known (crates/velt_loader/src/globals). Besides the
conditions on words (`export { … }` names, `// load when <module> uses: <words>`), a header may name
a *predicate* on a module's syntax (`// load for: a function expression with a this parameter`,
`// load for: an async host function`). Each says what a module does, never which std module that
loads; the header's imports say that. They go with (a2) (#2161 step 4).

Lowering has no runtime fallback for the integer hooks (#2197), and asserts that sema demanded each
one it calls (`hir::Program::hooks_demanded`). A float hook's conversion in std's own code, which
does not count yet, still goes to the runtime's printer.

## Diagnostics

Std code has no errors, so no message changes. A user module's own code is always checked, and
so is every std module it imports directly. An error in a deferred std function that a program
reaches is reported as before. One that nothing reaches is not reported, which only matters
while std itself is being written. What checks every std definition is `velt check --std`:
every file of std (a program may import only std's public modules, so no program reaches the
internal files nothing public uses), with laziness off (`CheckOptions::eager_std`).
`crates/veltc/tests/bench_programs.rs` `all_of_std_checks_without_diagnostics` runs it in CI and
requires no diagnostics, and nightly's `compile/check_all_std` measures it the same way.

## Cost

One more walk over the checked HIR, in exchange for not checking the unreached bodies; see the
pull request for the numbers.

**Accepted over the rule (#2345).** A program that converts a `number` no syntax marks as a float
to text loads and checks std's float printer (`number_float`, `format`, `shortest`, `text`,
`pow10`): about 14M instructions, `check` +4.59 % and `--emit vir` +7.98 % on a small program
(`class P { x: number = 3; }` printed in a template). That is the worst case of a change that
helps overall: hello and programs with no float text get cheaper, `all_std` stays within the
rule, and such a program's fractional field prints 54.7 % cheaper at run time. The cost was
profiled and has no hot spot, so it was accepted, with the decision on #2345
(https://github.com/velt-lang/velt/pull/2345#issuecomment-6695101845). Checking the printer once,
not in every program, is #2501.
