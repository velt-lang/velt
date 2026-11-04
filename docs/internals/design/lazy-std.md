# Standard library checked on demand

Status: implemented (#1658). Owner: `velt_sema` (`body/lazy_std.rs`).

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
diagnostics in std files need it. `VELT_EAGER_STD=1` turns laziness off for the command line too,
to compare.

## Loading on demand: `// load for:` predicates

Checking on demand still parses every module a program loads, and parsing and collecting std code
costs about as much as checking it. So some of std is not loaded at all unless a program may need
it: a global module under `std/prelude/global/` whose header names a condition
(crates/velt_loader/src/globals). Besides the conditions on words (`export { … }` names, `// load
when <module> uses: <words>`), a header may name a *predicate* on a module's syntax, evaluated by
the loader before any types are known. Each predicate is generic: it says what a module does,
never which std module that loads; the header's imports say that.

- `// load for: a conversion operand may be a float` (**Float operands**): a conversion (a
  template literal with an expression, `String(…)`, a `.toString(…)` call, or `+` / `+=` with a
  string literal or a template) whose operand looks like a float that is not a whole number:
  - a float literal (`1.5`, `1e-3`), `/` or `**` anywhere in it, a member or call of `Math`, or a
    cast to a float type (`f64`, `f32`, `f16`, `bf16`);
  - a variable or parameter declared with a float type, or initialized from such an expression
    (scopes are not told apart);
  - an array literal with such an element, and a `for … of` variable over one.

  `std/prelude/global/number_float.vlt` uses it to load std's float printer (the shortest
  digits, `velt/lang/hooks/number_float`), which lowering prefers to the runtime's.

std's integer printing (`velt/lang/hooks/number_text`) is loaded with the other hooks (the
prelude loads `velt/lang/hooks`), but its integer hooks are checked only where *live* code has a
construct whose lowering calls them (#2161 step 2; velt_common `hook_demand.rs`): a template or
`String()` of an integer, the formatting glue of a type that may print one (`console.log`, a
class's table), an array index (its out-of-bounds message) or an `any`. Live code is the
program's own and the std code it reaches (`velt_sema` `body/lazy_std/live.rs`), std's own
conversions included, so hello world checks none of std's integer printing. Lowering has no
runtime fallback for them (#2197), and asserts that sema demanded each one it calls
(`hir::Program::hooks_demanded`).

A predicate only decides what is loaded: a program it misses still compiles and prints the same,
by a slower path (a float the float-operand test misses is printed by the runtime's printer:
`tests/golden/lang/float_text_miss.vlt`, and `crates/veltc/tests/number_format_vir.rs` checks
which path runs), and one it wrongly matches only costs loading time. The principled fix is
loading after sema finds the use (std modules loaded lazily), which would retire the syntactic
test.

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
