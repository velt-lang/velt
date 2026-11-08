# Standard library checked on demand

Status: implemented (#1658; loading during the check, #2161 step 3; only live code). Owner: `velt_sema`
(`body/lazy_std/`, `std_load.rs`).

## Problem

The checker used to check every function body and module constant of every loaded module.
The prelude imports std modules that most programs never use (`velt/lang/number/parse` and its
1,300-line table of powers of five, `velt/encoding/json/reader`, `velt/time/*`), so hello world
paid to check all of them, although lowering builds only what `main` reaches.

## Design

A std body is checked once it is *live*, or once a checked call asks for its result. Live code
(`body/lazy_std/live.rs`) is a superset of what lowering builds, worked out by a walk over checked
code that checks each definition as it enters the set:

- the program's own definitions, and every module slot, whose initializer lowering runs whether
  or not code reads it;
- what lowering looks up by name ("Roots" below);
- closed over: every definition live code names (a call, a function value, a closure, a constant);
- the methods, static methods and constructors of each class or struct a live type mentions, with
  its fields' types, its base classes and its subclasses, transitively. A live type is the type of
  an expression, local or result in live code, or a type argument of one of its calls
  (`probe<Gadget>()`, whose body monomorphization builds with `Gadget`'s methods). Subclasses count
  because lowering's glue for a class value (printing, `JSON.stringify`, a member of `any`)
  dispatches over every class in the program that extends the value's class (`velt_vir`
  `any_subclass_dispatch`, `json_descendants`), constructed or not: printing an `Error` checks the
  methods of every `Error` subclass the program loads;
- the interface implementations of a live interface for a live type: lowering calls them through
  a bound or a table without checked code naming them. Every interface counts as live once live
  code holds an `any`, whose tables reach them;
- what no code names: a generator function's value is a `Generator` or `AsyncGenerator` whatever
  its written result says, a call of an async host function is awaited through a `HostSettled`,
  an `any` may become an `ArrayView` and may hold an `Error`;
- what sema makes after the bodies are checked: the implementations of interfaces through `any`,
  the conversions of its functions, the adapters of `toString` and `toJSON` (`live::finish`).

A deferred function that a call names is also checked before the call is (`body::returns::ret_of`,
which every call asks for the callee's result): what a caller's check learns from the callee's body
(its inferred result, what it keeps or modifies) is there, as when every body was checked first.
That can check a function the live walk never reaches: a body in a module checked whole calls it,
or a `catch` asks a dispatch group what it throws (`throws::own_now` checks each member). Such a
body is checked, not live: it adds nothing to the set, and lowering doesn't build it.

**Deferred** is every function with a body and every module constant, in a std module that isn't
checked whole. Module `let`s and static fields are never deferred: they are module state.

**Checked whole** is a std module that a user module imports directly, or uses as its JSX runtime
(`velt check` of a module that imports std reports everything in what it imports), or that has
top-level statements, which run when the module is evaluated.

### Roots

Lowering calls some definitions by name, which no checked code refers to:

- the free functions of `BY_NAME` (`std/lang/sort`, `std/lang/proxy`, `std/lang/typed_array`,
  `std/sync/epoch`, `std/lang/this_fn`, `std/host`);
- the free functions of the prelude's modules that are helpers (`HELPER_NAMES`: every `"__…"` name
  in the code of lowering, the optimizer and the backends, which a unit test checks, and the
  `__any<Type>_<method>` built-ins of `any` values);
- the hooks marked `lowered` that list no constructs (velt_common `std_hooks.rs`).

A root that takes or gives a value only live code can have (an array, a class instance, an `any`) is
live once live code has a value of that type (`live::gate`): lowering calls `stableSort` for an
array's `sort()`, `uncaughtHeader` for an `Error` thrown, `anyArr_push` for an `any`. One that
mentions an `any` waits for an `any`, whatever else it takes: `anyArr_push` also takes an array, and
every program that prints an integer has one, in a table of powers of ten. Methods need no root:
lowering calls them only where it has a value of their type, which makes them live.

Two kinds of hook wait for what the program does, because lowering can do without them: the record
hooks (`RECORD_HOOKS`, a conversion to a record whose hooks are unchecked stays a `TypeError`) and the
float hooks (`ON_DEMAND_FLOAT`, std's own text conversions use the runtime's printer where one is
unchecked). Their walk waits until `reach` finds that the program needs them.

### When the rules have a gap

What the walk never reaches gets a stub (hir_encodings.md "Unchecked definitions") and is listed in
`hir::Program::unchecked`. Lowering ICEs if it ever builds one, so a gap in the rules above is an
internal error on the first program that has it, not a changed program. A debug compiler (the one
the goldens run), or `VELT_LIVE_CHECK=1`, gives lowering the whole set too (`hir::Program::live`), and
each function it builds and each constant or module slot it reads must be in it (`Cx::assert_live`).
Glue that will call a std definition by name must be a root; a unit test scans the code of lowering,
the optimizer and the backends for `"std/<module>::<name>"` literals and checks that each one is.

The only places where lowering looks at `unchecked` instead of building are the two kinds of waiting
hook. That is why their triggers are in sema and not in the walk's closure, and why the walk marks
them live without checking them.

### What an unchecked body no longer contributes

Facts that used to come from every prelude body now come from live ones. A body that can't run has
no say in what live code does:

- A dispatch group joins the ownership modes of its members; a stub contributes its signature's
  modes (`ownership::mutation`). A member that isn't live can't be called, so a live member's
  modes, and a call through the group, need not allow for what it does. Whether a program's
  `velt check` reports a move can therefore depend on which implementations are live, the same way
  it always depended on which were loaded.
- A group's error type is the union of its members' at the end of `throws::infer_all`, stubs adding
  nothing. A `catch` reads it during the bodies' check, and that read checks every member first
  (`throws::own_now`), so a catch variable's type does not depend on liveness.
- Whole-program flags set by a body (`Ctx::writes_undefined`, `frozen_reads`, `primitive_guards`, …)
  are set by the live code that needs them. Their effect on live code is that of a pessimization
  gone (`frozen_reads`), or none.
- A static atomic of `std/sync/epoch` that nothing uses is a stub, not a `Global` that holds a
  reference, so a program with none has no `_Gstatics_drop`.

### What only the passes tell

Some of what lowering builds is known only after the passes that follow the bodies' check
(`absent_fields`, `any_iface_sites`, `type_level::instances`, `generic_fns::mark_shared_values`, …),
which don't run again, and some of which write what lowering reads. So a body they haven't seen isn't
checked there. The check runs again instead, with what it learned assumed from the start
(`live_constructs.rs` `Assumed`, `too_late`, `round: known too late`), and that is what
`velt build -v`'s sema rounds count:

- `live::finish` walks the code those passes make: an interface implemented through `any`, whose
  adapter converts a method's result to the interface's (`get(): Set<string>`, of which that may be
  the first mention), the conversions of `any`'s functions, the adapters of `toString` and `toJSON`.
  What it reaches and isn't checked is live from the start of the next round, a conversion to a
  record that it finds demands the record hooks there, and so do the hooks of a construct it finds.
  A next round finds a definition by name, since a module added during the check may number
  differently.
- The walk sees what code throws. Inference also gives a function what a call of a function value
  throws, and what the interface or the base class it implements declares, whatever its body does,
  and lowering reports those values when thrown and not caught. A value whose printing isn't
  demanded, or whose class isn't live, is assumed the same way.

The float hooks don't wait for that: their fallback prints the same. The editor doesn't run these
rounds. It checks every body already, and may differ from `velt check` in a module of the rare
program that demands one only this way.

### Everything else

The editor's analysis (`ide::check_for_ide`) checks everything, since hover, go-to-definition and
diagnostics in std files need it. `VELT_CHECK_ALL_STD=1` turns laziness off for the command line
too, to compare.

## Loading on demand

Checking on demand still parses every module a program loads, and parsing and collecting std code
costs about as much as checking it. So some of std is not loaded at all unless a program needs it.

**During the check (#2161 (a1)).** The modules of hooks no program needs by default
(velt_common `std_hooks::ON_DEMAND_MODULES`: `velt/lang/hooks/number_text`, `number_write`,
`number_float` and `any_record`) are not loaded with the prelude. `reach` finds out, from checked
code and its types, that one of their hooks is needed (a live construct's integer hook, `live.rs`;
a text conversion in the program's own code of a value that may hold a float, or a float printed
by live code, `number_text.rs`; a conversion to a record with string keys, `record_hooks_needed`),
and demands the module (`Ctx::demand_std`, `std_load.rs`):
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
  served. A collected definition keeps its `DefId` while the modules come in at the same points; a
  round that assumes definitions demands some earlier, so it finds them by name, and a hint by
  `DefId` holds only where its span matches.

`VELT_EAGER_STD=1`, and any caller of `load_program` that does not ask for `demand_std`, loads all
of these modules up front, after everything else, and gives the check no callback. Every golden's
`--emit vir` is byte-identical both ways (`crates/veltc/tests/eager_std_identity.rs`, selected by
`cargo xtask affected` when sema, the loader, lowering or std change): a difference is a rewrite an
added module missed, or something in the VIR that depends on the order sema numbers things in. VIR
does not name a `TyId` or a `FileId` for that reason (`Cx::view_key`, the `_L` of a
caller-tracking instance's symbol).

The editor runs the same demand set: `ide::check_for_ide` takes the callback, and with it works out
the live set (`Lazy::check_all`), though it checks every body.
Without the callback, a module the program did not load stays out and its hooks are not checked,
as with a std root that lacks it: `velt check`'s lint rules and `--ts-compat` analyse a program
loaded on demand that way, since they read only the program's own code.
`velt_lsp`'s `tests/demand.rs` requires the errors and the modules of `velt check`. `velt check
--std` and the editor's workspace index load everything, and demand nothing.

A demand must come before the passes that follow the bodies' check (`hooks::check_shapes` and the
others up to `live::finish`), which would not run for what it adds, so `Ctx::demand_std` from there
is an internal error, and `live::finish` leaves what it finds to the next round ("What only the passes
tell"); a float hook found there is left to the runtime's printer, as in std's own code.

std's integer hooks are checked only where *live* code has a construct whose lowering calls them
(#2161 step 2; velt_common `hook_demand.rs`): a template or `String()` of an integer, the formatting
glue of a type that may print one (`console.log`, a conversion to an interface, whose table holds
that glue), an array index (its out-of-bounds message) or an `any`. A class's own table has its
`format` slot only where the program formats (`Cx::format_demanded`), so a class or struct alone
demands nothing. A function may get a `throws` that needs formatting from what it implements
declares, which is known only after the passes, too late to load a hook: the check then runs once
more with `Format` assumed (`too_late`). Live code is a superset of what lowering builds: the program's
own and the std code it reaches (`velt_sema` `body/lazy_std/live.rs`), std's own conversions
included, so hello world checks none of std's integer printing. The four integer hooks act as one
bit: a program demands none of them or all of them (`live_constructs.rs` `demanded_for`).

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
while std itself is being written. An error about a user type inside a std generic needs the
instance, so the code that makes it: a user type gets into a std body through a call or a type
argument of live code, and the callee is live, with every method of the classes in it. What checks
every std definition is `velt check --std`: every file of std (a program may import only std's
public modules, so no program reaches the internal files nothing public uses), with laziness off
(`CheckOptions::eager_std`). `crates/veltc/tests/bench_programs.rs`
`all_of_std_checks_without_diagnostics` runs it in CI and requires no diagnostics, and nightly's
`compile/check_all_std` measures it the same way.

## Cost

One walk over live code, each definition once, in exchange for not checking the rest; see the pull
request for the numbers.

**Accepted over the rule (#2345).** A program that converts a `number` no syntax marks as a float
to text loads and checks std's float printer (`number_float`, `format`, `shortest`, `text`,
`pow10`): about 14M instructions, `check` +4.59 % and `--emit vir` +7.98 % on a small program
(`class P { x: number = 3; }` printed in a template). That is the worst case of a change that
helps overall: hello and programs with no float text get cheaper, `all_std` stays within the
rule, and such a program's fractional field prints 54.7 % cheaper at run time. The cost was
profiled and has no hot spot, so it was accepted, with the decision on #2345
(https://github.com/velt-lang/velt/pull/2345#issuecomment-6695101845). Checking the printer once,
not in every program, is #2501.
