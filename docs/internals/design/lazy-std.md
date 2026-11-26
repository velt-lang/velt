# Standard library checked and loaded on demand

Status: partly implemented. Issue: [#2161](https://github.com/velt-lang/velt/issues/2161).

## Summary

The checker checks a standard library body only once the program can reach it, and some std
modules are not even loaded until the check finds that the program needs one of their hooks. A
program pays to check the std code it uses, not the whole prelude: hello world checks none of
std's integer or float printing.

## Motivation

The prelude imports std modules most programs never use (`velt/lang/number/parse` and its table
of powers of five, `velt/encoding/json/reader`, `velt/time/*`). Lowering builds only what `main`
reaches, but checking every body of every loaded module would make hello world pay for all of
them. Parsing and collecting std code costs about as much as checking it, so for the modules that
hold only hooks, not loading them at all is the larger saving.

## Design

### Live code

A std body is checked once it is *live*, or once a checked call asks for its result. Live code
(`crates/velt_sema/src/body/lazy_std/live.rs`) is a superset of what lowering builds, worked out by
a walk over checked code that checks each definition as it enters the set:

- the program's own definitions, and every module slot, whose initializer lowering runs whether
  or not code reads it. A dependency package's definitions are live, as std's, once live code
  names them (they are checked whole either way), except the functions C calls (`@export`), which
  lowering builds whatever the program calls: what a package can do and the program never asks
  of it demands nothing (`http-server`'s TLS server, for options written without `tls`);
- what lowering looks up by name ([Roots](#roots));
- closed over: every definition live code names (a call, a function value, a closure, a constant);
- the methods lowering builds for each class or struct a live type mentions, with its fields'
  types, its base classes and its subclasses, transitively
  (`crates/velt_sema/src/body/lazy_std/live_methods.rs`): its constructor, the methods with a slot
  in its table (lowering fills every slot of a class's table), its getters and setters, the
  methods of its interface implementations, the methods glue calls by name (`toJSON`, `clone`, and
  the protocols sema's desugarings call) or by a symbol key (`[INSPECT]()`, std's keyed methods),
  and what sema made for it. Its `toString()` (own or inherited) is live where live code converts
  a value whose type holds the class to text (`String(x)`, a template, `+` with a string), every
  class's where such a value's type holds a type parameter, and an `Error`'s always: the text glue
  is its only caller. A method or static method with a plain name that no glue calls is live only
  where live code names it (a call, a function value), so a program that uses `Date` but never
  calls `toLocaleString` doesn't check it or load what it needs. Once the
  program reads, calls or writes a member of an `any`, the `any` tables call every class's methods
  by name (`Program::dyn_members`), and every method of a live class is live;
  `VELT_LIVE_ALL_METHODS=1` asks for that in any program. A live type is the type of
  an expression, local or result in live code, or a type argument of one of its calls
  (`probe<Gadget>()`, whose body monomorphization builds with `Gadget`'s methods). Subclasses count
  because lowering's glue for a class value (printing, `JSON.stringify`, a member of `any`)
  dispatches over every class in the program that extends the value's class, constructed or not
  (`velt_vir` `any_subclass_dispatch`, `json_descendants`): printing an `Error` checks those
  methods of every `Error` subclass the program loads;
- the interface implementations of a live interface for a live type: lowering calls them through
  a bound or a table without checked code naming them. Every interface counts as live once live
  code holds an `any`, whose tables reach them;
- what no code names: a generator function's value is a `Generator` or `AsyncGenerator` whatever
  its written result says, a call of an async host function is awaited through a `HostSettled`,
  and an `any` may become an `ArrayView` and may hold an `Error`;
- what sema makes after the bodies are checked: the implementations of interfaces through `any`,
  the conversions of its functions, the adapters of `toString` and `toJSON` (`live::finish`).

A deferred function that a call names is also checked before the call is
(`body::returns::ret_of`, which every call asks for the callee's result), so what a caller's check
learns from the callee's body (its inferred result, what it keeps or modifies) is the same as if
every body were checked first. That can check a function the live walk never reaches: a body in a
module checked whole calls it, or a `catch` asks a dispatch group what it throws
(`throws::own_now` checks each member). Such a body is checked, not live: it adds nothing to the
set, and lowering does not build it.

**Deferred** is every function with a body and every module constant in a std module that is not
checked whole. Module `let`s and static fields are never deferred: they are module state.

**Checked whole** is a std module that a user module imports directly or uses as its JSX runtime
(`velt check` of a module that imports std reports everything in what it imports), or that has
top-level statements, which run when the module is evaluated.

### Roots

Lowering calls some definitions by name, which no checked code refers to
(`crates/velt_sema/src/body/lazy_std/live_roots.rs`):

- the free functions of the modules in `BY_NAME` (`std/lang/sort`, `std/lang/proxy`,
  `std/lang/typed_array`, `std/sync/epoch`, `std/lang/this_fn`, `std/host`);
- the free functions of the prelude's modules that are helpers (`HELPER_NAMES`: every `"__…"` name
  in the code of lowering, the optimizer and the backends, which a unit test checks, and the
  `__any<Type>_<method>` built-ins of `any` values);
- the hooks marked `lowered` that list no constructs (`crates/velt_common/src/std_hooks.rs`).

A root that takes or gives a value only live code can have (an array, a class instance, an `any`)
is live once live code has a value of that type (`live_roots.rs` `gate`): lowering calls
`stableSort` for an array's `sort()`, `uncaughtHeader` for an uncaught `Error`, `anyArr_push` for
an `any`. One that mentions an `any` waits for an `any`, whatever else it takes: `anyArr_push` also
takes an array, and every program that prints an integer has one, in a table of powers of ten.
Methods need no root: lowering calls them only where it has a value of their type, which makes
them live.

### Hooks demanded by constructs

A hook whose entry in `std_hooks.rs` lists constructs is checked only where live code has one of
them (`crates/velt_common/src/hook_demand.rs`); `hir::Program::hooks_demanded` tells lowering
which were demanded, and lowering asserts that each hook it calls is among them. The integer
hooks are demanded by a template or `String()` of an integer, the formatting glue of a type that
may print one (`console.log`, a conversion to an interface, whose table holds that glue), an array
index (its out-of-bounds message) or an `any`. A class's own table has its `format` slot only where
the program formats (`Cx::format_demanded` in `crates/velt_vir/src/lower/hook_calls.rs`), so a
class or struct alone demands nothing. The integer hooks, `console.log`'s integer writers among
them, act as one bit: a program demands none of them or all of them (`live_constructs.rs`
`demanded_for`), so that glue, which lowering makes per type, does not depend on each call site
being listed for the right construct. The float text hooks (`numberText`, `numberPush`,
`numberInspectPush`) are a bit of their own, `TextFloat`: demanded by a conversion or glue of a
value that may hold a float, and by `Dynamic`. `console.log`'s float writers are another,
`WriteFloat`: demanded by a float printed (and by `TextFloat`). They hold no integer printing
(whole numbers have a writer of their own), so a program that prints only floats loads neither
the integer hooks nor the float text hooks.

Some hooks wait for what the program does, because lowering can do without them:

| Hooks | Demanded by | Where unchecked |
|---|---|---|
| record hooks (`RECORD_HOOKS`) | a conversion to a record with string keys (`record_hooks_needed`) | the conversion stays a `TypeError` |
| std's float printing inline at the program's conversions (`ON_DEMAND_FLOAT`) | a text conversion, in the program's own code, of a value that may hold a float (`number_text.rs`) | `numberText` / `numberPush`, which reach it by role, prebuilt |
| inspect shapes (`INSPECT_SHAPES`) | see [printing typed values](inspect-shapes.md) | lowering's format glue |

### Loading on demand

The modules of hooks no program needs by default (`ON_DEMAND_MODULES` in
`crates/velt_common/src/std_hooks/on_demand.rs`: the number printing and parsing hooks,
`any_record`, `unwind`, `inspect`, `inspect_cycles`, `pow`, the string searches and others) are
not loaded with the prelude. When the check finds that one of their hooks is needed, it demands
the module (`Ctx::demand_std`, `crates/velt_sema/src/std_load.rs`):

- The driver's load callback (`velt_sema::LoadFn`) continues the program's load
  (`velt_loader::load_more`) with the module and what it imports that is not loaded. Each such
  module must follow the rule of `velt_loader::std_index`, so what it adds stays local to it; the
  loader applies the rule to the parse at hand. A module that breaks it is an internal error
  (`crates/velt_loader/src/std_index/rule_tests.rs`).
- Sema runs the source rewrites on the batch alone, appends the modules, collects what they
  declare (`collect::collect_added`: every pass of `collect`, from where the additions start) and
  checks their eager bodies; then the walk goes on. Existing `DefId`s do not change.
- The batches outlive every `Ctx` and every check round of the program (`StdLoads`). A later round
  starts from the program's modules and takes the kept batches in the order they were served. A
  round that assumes definitions demands some earlier, so it finds them by name, and a hint by
  `DefId` holds only where its span matches.

A module that breaks the rule is not added in the middle of the check. With the driver's
`LoadOptions::refuse_out_of_rule` the loader refuses it (`StdBatch::refused`) instead of
reporting an internal error; the check stops there (`StdLoads::refused`: the bodies' walk and
the rounds end, every later demand is refused too), and the driver then throws that check away and loads the program again with the refused modules
up front (`LoadOptions::preload`, veltc's `sema_stage`; `velt check`'s recovering check and the
language server do the same). That second check is an ordinary one: a module loaded up front
may extend what it likes.

The TLS client and server (`std/net/tls/fd_client.velt`, `fd_server.velt`, with the TLS engine,
the certificate checks and the crypto under them) load on demand (#2392): velt/net's `listenTls`
and `connectTls` and `fetch` call them by role (`sched_tls_listen`, `sched_tls_connect`,
`sched_tls_handshake`), so only a program whose live code can listen or connect with TLS loads
them. None of it breaks the rule any more (#3634):
`std/net/tcp_sched.velt` reaches the TLS streams' operations through function values the server
or client installs before its first handle (`std/net/tls/fd_hook.velt`, `fd_reactor.velt`), not
through an `extend` of a type of its own; the error classes under `Error` and `IoError`
(`CryptoError`, `TlsFailure`, `CertificateRefused`) load up front with `fd_hook.velt`; and the
record layer makes its views without `u8[]`'s `subarray` (`std/lang/typed_array/u8_view.velt`),
whose `extend` of `Array` would break the rule. A call of a function value is checked against its
type alone, so a program that never loads the TLS code checks none of it. Likewise the engine
drives the server's handshake through an interface (`std/net/tls/server_steps.velt`), so a
program that only connects never loads the server's handshake. velt/simd's `extend`s of the
built-in vector types, which the record layer and the crypto use, may be added in the middle of a
check too: the rule allows an `extend` of a built-in vector or mask type without type parameters
or `implements`, which only adds methods to a type no module declares. It can still change code
checked before it, which may hold a vector without velt/simd (`velt/arch/x86`'s `Vector256` loads
only part of it): a method or operator looked up on the vector type that an earlier block answered
(a program's blanket `extend<T> T`, say) and the new block takes over, or that nothing answered
and the new block would. The checker records every name looked up through `extend` blocks on a
vector type, found or not, and where the new block declares one it refuses the module, so the
program is checked again with it up front, as for a module the rule refuses
(`crates/velt_sema/src/collect/vector_extends.rs`, `StdLoads::refuse_served`; the recovered check of
a file with syntax errors and the editor's check do the same). So every program that reaches TLS, through a
`fetch` that can reach `https:` (every `fetch` that follows redirects), `connectTls` or
`listenTls`, is checked once, with the TLS code added in the middle of the check.

std code may also call a function hook by its role, `hookCall("role", args…)`
(`std/lang/intrinsics/hook_call.velt`), instead of importing the hook's module. Checking the call
demands the module and then checks an ordinary call of the hook's definition
(`crates/velt_sema/src/body/expr/hook_call.rs`), so the call exists only once its body is checked:
a prelude method whose implementation is in a module of its own loads that module only in the
programs whose live code calls the method. The module must follow the same rule.

A demand must come before the passes that follow the bodies' check (`hooks::check_shapes` and the
others up to `live::finish`), which would not run again for what it adds, so `Ctx::demand_std`
from there is an internal error; `live::finish` leaves what it finds to the next round.

Before the check, a global module under `std/prelude/global/` whose header names a condition is
loaded by the loader before any types are known (`crates/velt_loader/src/globals/`). Besides the
conditions on words (`export { … }` names, `// load when <module> uses: <words>`), a header may
name a predicate on a module's syntax (``// load for: a function expression with a `this`
parameter``, `// load for: an async host function`). Each says what a module does, never which std
module that loads; the header's imports say that.

### Check rounds

Some of what lowering builds is known only after the passes that follow the bodies' check
(`absent_fields`, `any_iface_sites`, `type_level::instances`, `generic_fns::mark_shared_values`,
…), which do not run again and some of which write what lowering reads. A body they have not seen
is therefore not checked there. The check runs again instead, with what it learned assumed from
the start (`live_constructs.rs` `Assumed`, `too_late`), and `velt build -v` counts these rounds:

- `live::finish` walks the code those passes make: an interface implemented through `any`, whose
  adapter converts a method's result to the interface's (`get(): Set<string>`, which may be the
  first mention of `Set`), the conversions of `any`'s functions, the adapters of `toString` and
  `toJSON`. What it reaches and is not checked is live from the start of the next round; a
  conversion to a record it finds demands the record hooks there, and so do the hooks of a
  construct it finds. The next round finds a definition by name, since a module added during the
  check may number differently.
- The walk sees what code throws. Inference also gives a function what a call of a function value
  throws, and what the interface or base class it implements declares, whatever its body does,
  and lowering reports those values when thrown and not caught. A value whose printing is not
  demanded, or whose class is not live, is assumed the same way; a `throws` that needs formatting
  and is known only after the passes makes the check run once more with `Format` assumed.

The float hooks do not wait for a round: their fallback prints the same text.

### What an unchecked body does not contribute

Facts that whole-program analyses gather from bodies come from live ones. A body that cannot run
has no say in what live code does:

- A dispatch group joins the ownership modes of its members; an unchecked member contributes its
  signature's modes (`ownership::mutation`). Whether `velt check` reports a move can therefore
  depend on which implementations are live, as it depends on which are loaded.
- A group's error type is the union of its members' at the end of `throws::infer_all`, unchecked
  members adding nothing. A `catch` reads it during the bodies' check, and that read checks every
  member first (`throws::own_now`), so a catch variable's type does not depend on liveness.
- Whole-program flags set by a body (`Ctx::writes_undefined`, `frozen_reads`,
  `primitive_guards`, …) are set by the live code that needs them.
- A static atomic of `std/sync/epoch` that nothing uses is unchecked, not a `Global` that holds a
  reference, so a program with none has no `_Gstatics_drop`.

### The editor and full checks

The editor's analysis (`ide::check_for_ide`) checks every body, since hover, go-to-definition and
diagnostics in std files need them. It takes the load callback, so it loads the same modules and
works out the same live set as `velt check`; it does not run the extra rounds, so it may differ in
a module of a program that demands one only through them. `crates/velt_lsp/src/tests/demand.rs`
requires the errors and the modules of `velt check`. Without the callback, a module the program
did not load stays out and its hooks are not checked: `velt check`'s lint rules and `--ts-compat`
analyse a program that way, since they read only its own code. `velt check --std` and the editor's
workspace index load everything and demand nothing.

`VELT_CHECK_ALL_STD=1` turns lazy checking off on the command line, and `VELT_EAGER_STD=1` (and
any caller of `load_program` that does not ask for `demand_std`) loads every on-demand module up
front, after everything else, and gives the check no callback.

## Compilation

### When the rules have a gap

What the walk never reaches gets a stub (the "Unchecked definitions" section of
[HIR encodings](../contracts/hir_encodings.md)) and is listed in `hir::Program::unchecked`.
Lowering raises an internal error if it ever builds one, so a gap in the rules above is an ICE on
the first program that has it, not a silently changed program. A debug compiler (the one the
end-to-end tests run), or `VELT_LIVE_CHECK=1`, also gives lowering the whole live set
(`hir::Program::live`), and each function it builds and each constant or module slot it reads
must be in it (`Cx::assert_live` in `crates/velt_vir/src/lower/program.rs`). Glue that calls a std
definition by name must be a root; a unit test scans the code of lowering, the optimizer and the
backends for `"std/<module>::<name>"` literals and checks that each one is.

The only places where lowering looks at `unchecked` instead of building are the hooks with a
fallback. That is why their triggers are in sema and not in the walk's closure.

### Order independence

Every end-to-end test's `--emit vir` is byte-identical with modules loaded on demand and with
`VELT_EAGER_STD=1` (`crates/veltc/tests/eager_std_identity.rs`, selected by `cargo xtask affected`
when sema, the loader, lowering or std change). A difference is a rewrite an added module missed,
or something in the VIR that depends on the order sema numbers things in. VIR therefore names no
`TyId` or `FileId` (`Cx::view_key`, the `_L` of a caller-tracking instance's symbol).

## Runtime cost

There is no run-time cost. At compile time it is one walk over live code, each definition once,
in exchange for not checking or loading the rest; some programs need an extra check round.

A program that converts a `number` no syntax marks as a float to text loads and checks std's float
printer (`number_float`, `format`, `shortest`, `text`, `pow10`), about 14M instructions, which is
noticeable on a small program (`class P { x: number = 3; }` printed in a template). That is the
worst case of a rule that helps overall: hello world and programs with no float text are cheaper.

## Diagnostics

Std code has no errors, so no message depends on laziness. A user module's own code is always
checked, and so is every std module it imports directly. An error in a deferred std function that
a program reaches is reported; one that nothing reaches is not, which only matters while std
itself is being written. An error about a user type inside a std generic needs the instance: a
user type gets into a std body through a call or a type argument of live code, and the callee is
live, with every method of the classes in it.

`velt check --std` checks every file of std with laziness off (`CheckOptions::eager_std`); a
program may import only std's public modules, so no program reaches internal files nothing public
uses. `crates/veltc/tests/bench_programs.rs` `all_of_std_checks_without_diagnostics` runs it in CI
and requires no diagnostics, and the nightly benchmark `compile/check_all_std` measures it.

## Not yet implemented

- **Checking std's TLS once.** A program that reaches TLS still checks, lowers and builds the
  client (about 50,000 lines) itself; std's units (#2943) would check and compile it once per
  key and reuse it.

- **Global modules loaded on demand.** The global modules with a `load for:` predicate, such as
  `std/prelude/global/host_async.velt`, move from the loader's syntactic trigger to a demand made
  during the check. `host_async` cannot follow the on-demand rule today: `velt/host/promise` loads
  `velt/io/error`, whose `IoError` extends the prelude's `Error`, which the rule excludes.
- **Checking std modules once.** A cache of checked std modules removes the float printer's cost
  from every program that needs it ([#2501](https://github.com/velt-lang/velt/issues/2501)).
