# Standard library checked on demand

Status: implemented (#1658). Owner: `velt_sema` (`body/lazy_std.rs`).

## Problem

The checker used to check every function body and module constant of every loaded module.
The prelude imports std modules that most programs never use (`velt:number/parse` and its
1,300-line table of powers of five, `velt:json/reader`, `velt:datetime/*`), so hello world
paid to check all of them. Lowering builds only what `main` reaches, so none of that code ends
up in the program. Each such module made every program slower to compile: about 3% for
`velt:number/parse`, about 20% more for `velt:json/reader`.

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
  the methods of every `Error` subclass the program loads (their `__inspect`, `toJSON`, …).

The prelude (`std/prelude/`, `std/record`, `std/collections`) is lazy in a narrower way:
lowering looks up its types' methods and its helper functions by name, so only its free
functions and static methods are deferred, except the helpers (`HELPER_NAMES`: every `"__…"`
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

The JSON glue is the one exception, to keep its modules off hello world's bill: it calls the
methods of `std/json/reader`'s `Reader` and the functions of `std/json/writer` by name
(`velt_vir` `json_std_fn`), but only in the glue it builds for an instantiation of a JSON
intrinsic: the decoders (`JsonParse`, `JsonParseRows`, `JsonParseCursor`, `ReadRows`,
`ReadRow`) call the reader, the encoders (`JsonStringify`, `DbBind`) the writer. So those two
modules (`JSON_GLUE`) stay lazy, and each is reached whole (all of its deferred definitions at
once) at the first instantiation of one of its intrinsics in checked code
(`body/lazy_std_json.rs`):

- an intrinsic whose types are concrete is one;
- one whose types mention the type parameters of the generic function it is in
  (`JSON.parse<T>`'s `__intrinsic_json_parse<T>`) makes that function generic glue; so does a
  call of generic glue with type arguments that mention the caller's own parameters
  (`JsonValue.as<T>` calling `JSON.parse<T>`). A call of generic glue with concrete type
  arguments is an instantiation;
- conservatively, so is an intrinsic or a call in a function whose type parameters a call
  does not name (a closure, a method of a generic type, a default), and any call of a generic
  method through an interface or `any` once a generic glue function is a method.

A prelude definition that only this glue calls by name (one that builds a `JsonValue` from the
reader's state, say) can be reached the same way instead of staying checked as a helper:
`JSON_GLUE_PRELUDE` lists it with its module, and it is deferred, even as a method, and reached
with that module or by a reference in checked code (not by a use of its type).

So a prelude method that stays checked but that nothing calls (`JsonValue.as<T>`) costs hello
world nothing. What the reader and writer call in other modules (`std/json/scan`,
`std/number/*`) is reached from their code as usual. Eagerly checked std code that encodes a
concrete type (`JSON.stringify` in the typed arrays' and proxies' code) reaches the writer for
every program.

The editor's analysis (`ide::check_for_ide`) checks everything, since hover, go-to-definition and
diagnostics in std files need it. `VELT_EAGER_STD=1` turns laziness off for the command line too,
to compare.

## Diagnostics

Std code has no errors, so no message changes. A user module's own code is always checked, and
so is every std module it imports directly. An error in a deferred std function that a program
reaches is reported as before. One that nothing reaches is not reported, which only matters
while std itself is being written. What checks every std definition is `velt check
bench/compile/all_std.vlt` with `VELT_EAGER_STD=1`: all_std.vlt imports every module, which makes
them all eager except the prelude, whose free functions and statics would stay deferred.
`crates/veltc/tests/bench_programs.rs` `all_of_std_checks_without_diagnostics` runs it in CI and
requires no diagnostics, and nightly's `compile/check_all_std` measures it the same way.

## Cost

One more walk over the checked HIR, in exchange for not checking the unreached bodies; see the
pull request for the numbers.
