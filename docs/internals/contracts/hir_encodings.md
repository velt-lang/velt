# HIR encodings (contract detail for `crates/velt_sema/src/hir/`)

How sema encodes language features in HIR; lowering relies on every point here.
Maintainer-owned, like `hir/mod.rs`, `hir/intrinsic.rs` and `hir/module_state.rs`.

## M2 additions
(see docs/reference/classes.md and docs/reference/memory.md):
- Class instances are heap-allocated and referenced by a pointer; `Option<Class>` uses null.
  `new C(args)` is `ExprKind::New`: lowering allocates and calls the constructor (a `Def::Fn`
  whose first param is `this` with `PassMode::BorrowMut`). `super(args)` in a constructor is a
  root-level statement of its body: a `Call` of the base constructor with `Upcast(this)`, or
  `Lit(Unit)` when no ancestor has a constructor. `super.m(args)` in a method is a
  `Callee::Def` call of the base implementation with `this` (upcast) as `args[0]`; the call
  expression's span is in `Program::super_calls` (#823: JavaScript does not read `m` from
  `this` there, so a typed proxy's `get` trap does not run). Field defaults (`FieldDef::default`) run
  in JavaScript's order: a constructor evaluates those of its class's fields that its base
  constructor's class does not have, right after the `super(args)` call (when no ancestor has
  a constructor: after the `Lit(Unit)` statement in a derived class, on entry in a base
  class); `new` evaluates the rest (those of the classes below the one declaring the
  constructor, or all of them without one) after the constructor returns. Sema's throw sets
  follow the same split. `FieldDef::default` is always the declaring class's initializer;
  `AdtDef::inherited_inits` (additive) lists the initializers classes give fields they
  inherit (an error class's `name = "AppError"` sets the prelude `Error`'s `#name`), the base
  classes' first, each with its field, the declaring class's depth (0: no base class) and the
  own field `at` it runs before (that class's field count: after its last). They belong to
  the split of the declaring class: whoever runs that class's initializers runs each of them
  as `this.<field> = init` (dropping the old value) right before field `at`'s, in list order.
  Sema checks every initializer (`FieldDef::default`, `FieldInit::init`) and every parameter
  default a call fills in in a frame of its own, outside any function body: the locals it
  uses (its own temporaries, such as the one a `Record` literal fills) are numbered from 0
  there and belong to no body. Wherever lowering places one, it renumbers them after the
  locals of the body it runs in (`velt_sema::rebase_detached_locals(e, base)`, which also
  returns how many there are) and declares each at its `let` or binding pattern, which gives
  its type; sema puts no `let` without an initializer and no `catch` local in one, which
  would have none. A default sema inserts at a call (`FnInfo` parameter defaults, struct
  field defaults in object literals) already has locals of the calling body. A parameter
  default the function applies itself (a dynamic or `T | undefined` parameter's, which
  includes one using an earlier parameter, `this` or a closure, typed `T | undefined` by the
  parser) is checked in the function's body instead (`default_prologue`), as the initializer
  of the `let` that hides the parameter at the start of the body, with locals of that body.
- Virtual dispatch only for methods overridden somewhere: `Callee::Virtual { slot }` indexes
  `AdtDef::vtable` of the receiver's dynamic class; all other method calls are `Callee::Def`.
- Interface values (`Shape[]`) are `TyKind::Dyn`: fat pointer (data, vtable). `ExprKind::ToDyn`
  builds one from a concrete value using `Program::impls[impl_index]`; `Callee::Dyn { slot }`
  calls through it (slot = index into `InterfaceDef::methods`). A Dyn owns a heap box of its value.
  An impl's `ty` may be any type: builtins (`extend` blocks implementing `Comparable<T>` or
  `Iterable<T, E>`: `i64`, `T[]`, `string`, `Map<K, V>`) and interface value types
  (`IterableIterator<T, E>` implements `Iterable<T, E>`; its `ToDyn` boxes the fat pointer).
- Generic bounds (`T extends Shape`): method calls on a `TyKind::Param` receiver are
  `Callee::ParamMethod`; lowering picks the concrete method via `Program::impls` after
  monomorphization (static dispatch).
- Function values (`TyKind::FnPtr`) are closures: `{ code: Ptr, env: Ptr }`; `env` is null for
  named functions. `ExprKind::Closure(def)` captures per `FnDef::captures`: Borrow/BorrowMut
  captures store pointers (non-escaping closures), Copy/Owned captures store values (escaping).
  A non-escaping closure is a direct call argument, called immediately, or the initializer of a
  `StmtKind::Let` whose local is used only as the callee of `Callee::Indirect` calls in the same
  (non-async, non-generator) function (`const f = () => this.n; f()`): its env may live in the
  frame, and a call through the local borrows the captured variables.
  An Owned capture with `Capture::share` stores a share (`Intrinsic::Share` semantics) and
  leaves the enclosing local initialized (a shared value still used after the closure is created;
  see "Sharing"). A capture whose local is `LocalDef::boxed` stores the cell pointer instead.
  Closure bodies take the env pointer as a hidden first param; `code` has that signature.
  Members of a recursive closure group (nested functions calling each other, sema
  `nested_closures`) capture the same variables in the same order, so their envs have one
  layout; inside a member, `Intrinsic::ClosureSibling(def)` is the function value of member
  `def` with the running closure's env (a new reference to it), or, in an async member, a new
  heap env filled from its own captures (shared). Members are escaping (counted heap envs).
  In a sync member, a local holding a `ClosureSibling` that is only ever called is never made:
  lowering calls member `def`'s code with the running env directly.
- Errors: see "Errors" below.
- `x ?? d`, `a?.b`, `if (x != null)` narrowing are desugared by sema into `Match` with
  `PatKind::Some/None`; narrowed uses read the payload via `ExprKind::UnwrapSome`.
- Closures share the enclosing function's type-parameter numbering.
- Interface fields are read through synthesized getter methods: sema adds one `Def::Fn` per
  (impl, field) returning the field (Copy → copy, else borrow-as-clone) and appends it to the
  interface's method slots, so `x.field` on a `T extends I` / `Dyn` receiver is a
  `ParamMethod`/`Dyn` call. An interface the program writes a field through also has one
  setter slot per writable (non-`readonly`) field after the getters (slot `methods + fields +
  k`, `k` counting the writable fields before it; `InterfaceDef::methods` lists them as
  `<set f>`): a `Def::Fn` per (impl, field) taking `this` mutably borrowed and the value
  owned (Copy → copied), which moves it into the field; the caller shares a value it uses
  again, as for a class setter. `x.f = v` is a `ParamMethod`/`Dyn` call of that slot with
  arguments `[x, v]`; lowering holds the receiver of a `Dyn` setter call against `v` (a
  reference of its own when `v` may replace or free what `x` holds). Interfaces nobody writes
  through have no setter slots.

## M2 encodings sema emits
- Closure `FnDef::params`: one param per capture first (local = `Capture::inner`, mode = capture
  mode), then the declared params; body locals are numbered the same way. The closure value's
  `FnPtr` type lists only the declared params. `FnPtr` values are not Copy. Declared closure
  params are `Copy` (Copy types) or `Borrow` — never `BorrowMut`, even when the body modifies
  them (then `LocalDef::mutable` is set): a function value may be called with aliasing
  arguments.
- Method-call receivers (`args[0]`) use the `this` mode: `Borrow`/`BorrowMut` even for Copy types,
  `Move`/`Copy` for an inferred `Owned` `this`.
- `AdtDef::ctor` may be inherited from a base class (that ctor's `this` is the base type).
- Class vtable entries are always class methods (sema synthesizes a forwarder `C.m` when a
  subclass overrides an interface default that `C` inherits). An `ImplDef` entry whose method has
  a vtable slot in the implementing class is a synthesized trampoline `C.<dyn m>` doing a
  `Callee::Virtual` call.
- `Match` in statement position has type `Unit` and arms of any type.
- Borrow-mode pattern bindings refer into the scrutinee place; when any binding moves, the
  scrutinee place is `Move`. Partial moves: `Field { mode: Move }` / `UnwrapSome(_, Move)` out of a
  struct/option local (the other fields still need dropping).
- Negative integer literal patterns: `Lit::Int` holds the two's-complement bit pattern in the
  scrutinee's width (`-1` on `i8` → `0xff`); ranges compare signed/unsigned per `Pat::ty`.
- `Index::index` is always `usize`. Anonymous object ADTs are generic over the type params their
  fields mention. `x == null` as a value is `Match { None => true, _ => false }`; `??`/`?.` yield
  owned values. `Map<K, V>` is the prelude class (`TyKind::Map` is unused). `ToString`/`Print`
  accept every printable type (not function or interface values).

## M3 additions
- `FnDef::is_async` functions (and async closures) lower to state machines per
  docs/internals/contracts/rt_abi_async.md; `ExprKind::Await` suspends. Params of async fns are always
  `PassMode::Owned` or `Copy` (sema guarantees). A compiled async call is embedded when it is
  the direct operand of `Await` and given its own task when it is the direct operand of
  `Intrinsic::Spawn`; any other call of an async function (or of a function value returning a
  `TyKind::Promise(T)`) yields a started promise: boxed via `velt_rt_fut_box`, then
  `velt_rt_fut_start` (hybrid promises, docs/reference/async.md).
- `Intrinsic::SpawnHandled` (`__intrinsic_spawn_handled(p)`, standard library only) is
  `Intrinsic::Spawn` whose rejection the caller handles itself (`TaskScope.spawn`: the scope
  fails with it). A dropped `Spawn` handle reports its task's rejection as uncaught; a dropped
  `SpawnHandled` handle drops it quietly.
- Hybrid promises: sema rejects an expression statement of type `Promise<T>` or `Promise<T>[]`
  other than a `spawn(...)` call ("floating promise"). `Intrinsic::PromiseRace`
  (`Promise.race(ps: Promise<T, E>[]): Promise<T, E>`, `ps` owned) and the std-only
  `Intrinsic::PromiseAny` (`__intrinsic_promise_any`, same signature: the first to fulfill, or
  the last rejection). `Promise.allSettled(ps)` and
  `Promise.any(ps)` are `Call { Def(promiseAllSettled / promiseAny, [T]) }` of the prelude's
  async functions (std/prelude/promise.vlt), so a directly awaited `Promise.any` throws its
  `AggregateError` like any async call. `new Promise<T, E>(arrow)` is a `Call` of the prelude's
  `promiseNew` (or `promiseNewResolveOnly`) with two more arguments: the compiler-internal
  `Intrinsic::SourceLocation` (no arguments; lowered to the `"path:line:col"` string of its
  span) and a `bool` literal, true when the `new Promise` is the direct operand of `Await`.
  `Promise.withResolvers<T, E>()` is `Call { Def(promiseWithResolvers, [T, E]) }`.
- A promise used where a promise type with a wider error type is expected (`Promise<T>` or
  `Promise<T, E1>` where `Promise<T, E2>` is expected, every error of `E1` allowed by `E2`) is
  `Call { Intrinsic(PromiseWiden), [p] }`, `p` owned, typed as the expected promise type. The
  wrapper is lazy; used as a value (not awaited or spawned right away), it starts `p`.
- `async main` → `velt_main` calls `velt_rt_block_on`.
- std wraps rt I/O with `declare async function` externs (`ExternFnDef::is_async`); rt results
  use `IoResult` layout = Velt `struct { code: i32; message: string; value: T }`.

- The prelude (`std/prelude/*.vlt`) is ordinary Velt code; methods in `extend` blocks are
  `Def::Fn`s whose `self_ty` is the extended (possibly builtin) type.
- `Intrinsic::HoldsNull` (`__intrinsic_holds_null(probe)`, std only) is a constant per
  instantiation: whether the probe's type is an option once substituted, so that a generic
  `T | null` is `T` itself. `Intrinsic::NullAs` (`__intrinsic_null_as<T>()`, no arguments, type
  `T`) is that `T`'s `null`; std calls it only where `HoldsNull` is true (lowering of any other `T`
  is a run-time internal error). Lowering evaluates a condition built from `HoldsNull` with `!`,
  `&&` and `||` itself (`const_cond`): an `if` lowers only the branch taken and `&&` / `||` only
  the operand that decides, so the code of an instantiation for which the test is false is the
  code without it (std/prelude/map.vlt: a `Map` whose `V` excludes null keeps its VIR).
- Type predicates on a type parameter (issue #1251): `if (g(x)) A else B`, where `g` is a type
  predicate `v is P` whose `P` has only primitive members and `x` has the type of a type
  parameter `T`, has the condition `Logical { And, g(x), Call { Intrinsic(MayHoldPrimitive),
  [x] } }`. `MayHoldPrimitive` is a constant per instantiation: `false` when `T` is a struct,
  class, object type, array, tuple, map or function type. Where it is `false`, the `if` evaluates
  `g(x)`, stops with a `TypeError` if that returned `true` (the predicate was wrong), and runs
  only `B`; `A` is not lowered for that instantiation (a generic function that calls itself as
  `signal({ value: x })` in `A` has finitely many instantiations).
- Text of a type parameter (issue #1462): `String(x)`, a template part `${x}` and a string
  `+` operand `x` of the type of a type parameter `T` are `If { cond: Call {
  Intrinsic(HoldsDynamic), [Call { Intrinsic(NullAs), [] }: T] }, then: <x converted to any,
  written as `String(x)` (the prelude's `__anyString`) or a template part / `+` operand of an
  `any` writes it>, els: Call { Intrinsic(ToString | TemplateToString), [x] } }`, each branch
  with its own copy of `x`. `HoldsDynamic` is a constant per instantiation: `true` when `T` is
  `any`, `unknown` or `object`, or an array, tuple, union or nullable type holding one; a `+`
  operand tests `HoldsDynamicOrClass` instead, also `true` for a class (its `valueOf()` comes
  first). The probe is never evaluated. Lowering evaluates it like `HoldsNull` (`const_cond`),
  so each instantiation lowers one branch, and the others the code they had without the test.
- A value of a type parameter passed for a union parameter with that type parameter as a member
  (issue #1456; sema `param_member.rs`): the argument `Variant { [x] }` (or `WrapSome(x)`,
  `WrapSome(Variant { [x] })` for an option), `x` a variable or field path, is `If { cond:
  HoldsDynamic(NullAs: T), then: <x converted to any and routed to the member whose test it
  passes, as an `any` converted to the union is>, els: <the wrap> }`. The routing reads `x`
  once per test and converts it once in each arm (no temporary); where the wrap moves `x`,
  every arm moves it. Lowering's drop-flag scan (`FlagScan`) looks only at the branch the
  instantiation takes, and `velt_sema::effects::routed_wrap` gives the effects of such an
  argument as the wrap's.

## M3/M4 encodings (lowering assumptions, confirmed at merge of ir-3)
- An async fn's `FnDef::ret` is the body's return type `T` (the call's type is `Promise<T>`).
- `spawn` accepts a promise value or an async closure. Promise errors: see "Errors" below.
- `Mutex<T>` is laid out as `struct { lock: u64; value: T }`. `Mutex<scalar>.with` gets a copy.
- `JsonError` and `JsonValue` are resolved by name in the prelude. Optional fields are
  `T | undefined` fields (`T | null | undefined` for `a?: T | null`) with an `undefined`
  default (#930 phase 2).
- An async closure clones its owned captures into each promise it creates (it may be called many
  times, e.g. as an HTTP handler); borrowed captures are read from its env. A local async
  closure (`FnDef::shares_captures`, #208: sema proved it never reaches a thread boundary)
  instead gives each promise another reference to its owned captures (a share, as a generator
  closure does), and the cell itself for a `LocalDef::boxed` capture.
- `__intrinsic_http_handler(closure)`: `Call { Intrinsic(HttpHandler), [closure] }`, type `u64[]`
  (init, poll, drop, state_size, state_align, env).
- Sema rejects: JSON of maps without `string` keys / functions / interface values, and
  `JSON.parse` of unions whose members it cannot tell apart; atomics on non-64-bit ints.

## Post-M4 additions
- `StmtKind::ForOf { consume: true, .. }`: `for...of` over an owned temporary array (a call
  result, `await …`, an array literal — never a place) of non-Copy elements. `iter` is lowered
  as an owned value; `binding` is an owned pattern (`UseMode::Move` bindings, as in `let`), and
  every element is moved into it in order. When the loop is left early (`break`, `return`, a
  throw) the elements not reached yet are dropped; then the buffer is freed without dropping the
  elements that were moved out. `consume: false` is the borrowing loop (elements borrowed, or
  copied when Copy).
- `for...of` over an iterable (a type with `[Symbol.iterator]()`, docs/internals/design/
  iteration.md) has no HIR form of its own: sema emits a `Block` with `let <iterator@N> =
  src[Symbol.iterator]()`, a `<open@N>` flag and a `Try { finally }` around a `While` that calls
  `next()`, binds `value` and runs the body; the `finally` calls `return()` when the body was
  left early (a direct generator call has its own form, "Generators"). `StmtKind::ForOf` stays
  the array loop. Locals named `<…>` are compiler-made.
- Exclusive access (docs/reference/memory.md) is checked by sema; lowering relies on it for VIR
  parameter attributes (vir.rs invariant 9, `noalias` etc.).

## Generators
(docs/reference/functions.md "Generators", docs/internals/design/iteration.md §3)
- `FnDef::is_generator`: a `function*` / `*name()`. `FnDef::ret` is its declared result —
  `Generator<T, E>` (the prelude class `std/prelude/iter::Generator`), `Iterator<T, E>` or
  `Iterable<T, E>` — with `E` = `FnDef::throws` (or `never`); `T` is the yield type. A call of
  it (`Callee::Def`, also through `FnRef` values and vtables) has type `ret`, never throws, and
  only creates the generator; lowering builds a `Generator` object and, for an interface
  `ret`, the interface value of it. Params are `Owned` or `Copy` (methods: `this` too), as for
  async functions.
- `Generator<T, E, N>`: `N` (`never`: none) is the type `next(value)` passes in. In the body:
  `Call { Intrinsic(Yield), [v] }` (`v: T` owned, type `Unit`) is `yield v`; in a generator
  with an `N` whose value is used, the call has type `N` and may have a second argument (owned,
  evaluated only when resumed by a `next()` without a value: `undefined` as an `N`; without
  it such a resume panics). `Return(None)` ends the generator (no `Return(Some)` occurs);
  `yield* src` has no form of its own: a `Block` expression holding the desugared `for...of`
  that yields each value or, in a generator with an `N` or a program with
  `Program::generator_throw`, for a `src` of type `Generator<T, E2, N | never>`, `Block { Let
  <yield-gen@N> = src (or GeneratorEmbed(src)); If { cond: GeneratorResume(<yield-gen@N>),
  then: [While { cond: YieldDelegate(<yield-gen@N>), body: [] }] } }`. With
  `Program::generator_throw`, a `Yield` or `yield*` inside a `try` with a `catch` may throw an
  `any` (what `throw()` passes in), and the catch types include it. No
  `finally` block of a generator contains a `Yield`, can throw, or contains a `Break` /
  `Continue` whose target loop is outside it (sema).
- The `Generator` class's methods (prelude) use `Intrinsic::GeneratorResume` (`bool`, throws
  `E`), `GeneratorResumeWith` (`next(value)`; the value `Borrow`), `GeneratorThrow` (`u32`
  code, the error an `any`, `Borrow`), `GeneratorValue` (`T`, moved out; only after a resume
  returned true) and `GeneratorReturn` (close), each taking the `Generator<T, E, N>` operand
  `BorrowMut`. `Program::generator_throw` says whether a user module calls a `throw()`
  method; without it lowering emits no code for errors thrown in.
- `for...of` over a direct generator call (`gen(a)`, or the generator `[Symbol.iterator]()`
  method it would call) is `Block { Let <generator@N> = Call { Intrinsic(GeneratorEmbed),
  [call] } : Generator<T, E>; While { cond: GeneratorResume(<generator@N>), body: [LetPat
  pattern = GeneratorValue(<generator@N>), Block(body)] } }`: lowering keeps the generator's
  state in that local (the local is only an operand of the generator intrinsics and is
  dropped at the block's end, which closes it). A `GeneratorEmbed` it cannot embed (a
  generator iterating a direct call of itself) is the call's `Generator` object.
- A generator function expression (`function* (...) { ... }`, also `async function*`) is a
  closure (`ExprKind::Closure`) whose `FnDef` has `is_generator` (and `is_async`) and
  `captures`, all by value (`Owned` / `Copy`): calling the closure value creates the generator,
  whose state holds another reference to each capture (a share, not a copy: generators stay on
  their thread), and the cell itself for a `LocalDef::boxed` capture (a variable assigned after
  the capture, by either side). Its function type's result is `ret` with the
  final `E` and it does not throw.

## Async generators
(docs/reference/functions.md "Async generators", docs/internals/design/iteration.md §4)
- An `async function*` / `async *name()` has `FnDef::is_generator` **and** `FnDef::is_async`.
  `ret` is `AsyncGenerator<T, E>` (prelude class `std/prelude/iter::AsyncGenerator`),
  `AsyncIterator<T, E>` or `AsyncIterable<T, E>`, with `E` = `FnDef::throws`; a call has type
  `ret`, never throws and creates the generator (an `AsyncGenerator` object, or the interface
  value of it). The body contains `Await`s and `Yield`s; its `finally` blocks follow the
  generator rules above but may contain `Await`.
- The `AsyncGenerator` class's methods use `Intrinsic::AsyncGeneratorResume` (type
  `Promise<bool, E>`; only as the operand of `Await`, which rejects with `E`),
  `AsyncGeneratorValue` (`T`), `AsyncGeneratorReturn` (`Promise<void>`, only under `Await`:
  the awaited close) and `AsyncGeneratorDispose` (the close without awaiting), each taking the
  `AsyncGenerator<T, E>` operand `BorrowMut`.
- `for await` has no HIR form of its own: over an async iterable sema emits the `for...of`
  protocol block with `Await(next())` and, in the `finally`, `Await(return())`. Over a direct
  async generator call it is `Block { Let <generator@N> = Call { Intrinsic(GeneratorEmbed),
  [call] } : AsyncGenerator<T, E>; Try { body: [While { cond: Await(AsyncGeneratorResume(
  <generator@N>)), body: [LetPat pattern = AsyncGeneratorValue(<generator@N>), Block(body)]
  }], finally: [Await(AsyncGeneratorReturn(<generator@N>))] } }` (the close is a no-op once
  the generator is done). Over a sync source it is the `for...of` form whose bound value is
  `Await(value)` when the elements are promises; an array of promises is a consuming `ForOf`
  (its place operand `UseMode::Move`).
- `yield* src` in an async generator is the desugared `for await` that yields each value.

## Mutation inference (no `mut` in the language)
- Pass modes are inferred (docs/reference/memory.md): `BorrowMut` = the callee may
  modify the value behind the pointer; it is never assigned as a whole (reassigned non-Copy
  params are `Owned`). `BorrowMut` has the same ABI as `Borrow` for every type (aggregates by
  pointer, scalars — class objects, `Class | null`, `shared<T>` — by value); only the attributes
  differ. `Copy` params are values: lowering gives a Copy aggregate param that the body modifies
  (`LocalDef::mutable`) its own copy at entry, since borrow-ABI calls pass a pointer to the
  caller's value.
- Args of calls follow the callee's final modes: `Callee::Def`/`New` the def's, `Virtual` the
  vtable entry's (sema joins the modes of all methods sharing a vtable slot), `Dyn`/`ParamMethod`
  the join over the interface's default and implementations (implementations keep their own
  modes; lowering's `dyn_modes` joins them the same way). `Callee::Indirect` args are `Borrow`
  uses (the callee may still modify objects through them: closure params carry no no-alias or
  read-only guarantee).
- Params of named functions used as values (`ExprKind::FnRef`) are `Copy`/`Borrow`/`Owned` only:
  sema demotes an inferred `BorrowMut` to `Borrow` + `LocalDef::mutable` (direct calls still pass
  `BorrowMut` uses). For every param, `LocalDef::mutable` means the body assigns or modifies it;
  lowering never marks such a param `readonly`.
- `TyKind::FnPtr` has no mutability information: `{ params, ret, throws, rest }`.
- `TyKind::FnPtr::rest`: the last of `params` is a rest parameter (an array type,
  `(a: A, ...xs: T[]) => R`). Sema packs a call's remaining arguments into that array (and
  passes `[]` when there are none), so every call through the value passes `params.len()`
  values and its ABI is that of the same type with `rest: false`; lowering may ignore the flag.
  A rest parameter typed by a tuple is never interned: `Types::intern` spreads the tuple into
  `params` and clears `rest` (`(...a: [A, B]) => R` is `(a: A, b: B) => R`).
- `declare function` params are never inferred modified: std passes out-parameters as fresh locals.

## Generic methods
- Generic interface methods: `InterfaceDef::methods[slot]` as usual; the method's own type
  params follow the interface's params and the implementor (`Param(n + 1 + k)`).
  `Callee::ParamMethod::method_type_args` carries their arguments; lowering appends them to
  the implementing method's owner type args (`Program::impls` entry, a generic `Def::Fn`).
  Sema never emits `Callee::Dyn` for them; interface vtables leave their slots empty.
- `x instanceof C` where `x` is a base class of `C`, an interface value, or a union member of
  such a type: a `Match` whose arm pattern is `PatKind::InstanceOf(C)` (inside `Some` /
  `Variant` patterns as for any member test). It matches when the dynamic class is `C` or a
  subclass of `C` (type arguments are not compared). Lowering numbers all classes in a
  pre-order walk of the hierarchy, so `C` and its subclasses have the ids `lo..=hi`, and every
  vtable's first word (slot -7) holds the class id of its concrete type: 0 for non-classes;
  in an interface table of a class whose objects carry a vtable pointer, `u64::MAX` (read
  the object's own table, which may be a subclass's).
- A local (or a path of `readonly` fields) narrowed by such a test reads as
  `ExprKind::Downcast(read)` typed as `C<args>`. It is a place like its operand (same use
  mode, no projection of its own); on a class it is the same pointer, on an interface value
  the data pointer. Nothing is checked at run time.
- Generic class methods never get a vtable slot (an `override` of one is recorded by sema only);
  calls are `Callee::Def` on the static class. Sema rejects calls through a class that has a
  subclass overriding the generic method.

## Union types
- `A | B | C` (two or more distinct non-null members) is `TyKind::Adt(d, args)` where `d` is a
  compiler-generated `Def::Enum` with `EnumDef::is_union = true`: one variant per member, in
  canonical order, each with exactly one payload (the member type); discriminant = variant index.
  Canonical form: nested unions and `| null` flattened, members deduplicated, `never` dropped,
  sorted (types without type parameters first, then by interned `TyId`), so `A | B` and `B | A`
  are the same `TyId`. The enum is generic over the type parameters its members mention
  (renumbered by first occurrence, `args` = the outer params), like anonymous object types.
  `void` members are rejected. Exactly one non-null member is that type itself (`T | null` stays
  `Option<T>`); `null` plus several members is `Option<union>`.
- Widening a member value: `ExprKind::Variant { def, type_args, variant, args: [value] }`; the
  value keeps its use mode (a borrowed place is shared by lowering, as for `WrapSome`).
  Union → union with more members, and `T | null` → `U | null`: a `Match` re-tagging each variant
  (`Variant(b) => Wider.Variant(b)`); variants ruled out by flow narrowing get an arm
  `_ => panic("unreachable union member")`.
- Flow narrowing to one member: a read of the local is
  `ExprKind::UnwrapVariant { expr: Local (or UnwrapSome(Local)), variant, mode }` — a place
  projection like `UnwrapSome` (base `Borrow`/`BorrowMut`, outer `mode` as for `Field`).
  Moving out of an `UnwrapSome`/`UnwrapVariant` chain rooted at a local moves the whole local
  (the payload is all it owns); lowering treats it like `Local(_, Move)`.
- `typeof x === "tag"`, `x instanceof C` and `x == literal` on unions are `Match`es yielding
  `bool` with `PatKind::Variant { args: [Wildcard | Lit] }` (and `None` for `null`) arms (a
  literal-type member is matched by `Wildcard`: it has one value). `typeof x` as a value is a
  `Match` yielding string literals (a constant `Lit::Str` on non-union types).
- A union converts to a type every member it can hold converts to (a union of literals to their
  base type, members to a common base class / interface): a `Match` whose arms convert each
  member (`V(m) => <m as T>`); re-tagging into a wider union may convert members the same way.
- `Print`/`PrintErr`/`ToString`/`JsonStringify` of a union format the active member (top-level
  style for `console.log` args: a string member prints raw). `JSON.parse` into a union
  picks the member by the JSON value (velt_vir lower/json/union.rs); sema rejects unions whose
  members it cannot tell apart.
- Clone/drop/eq/hash glue is the ordinary enum glue.

## Literal types
- `TyKind::Literal(LitValue)` (`"circle"`, `42`, `true`): zero-sized — lowering maps it to
  `Ty::Unit` (no bits in fields, payloads or locals; struct/variant layouts skip it). A
  literal-typed value is `Lit(Lit::Unit)` with the literal type (never a `Lit::Str`/`Int`).
- Conversion to the base type (`string`, the number type, `bool`) is explicit in HIR: sema
  replaces the value by the base-typed constant (`Lit`, `Unary(Neg, Lit)` for negatives), keeping
  a non-place operand for its effects (`Block { [Expr(e)], value: Lit }`).
- `Print`/`PrintErr`/`ToString`/`JsonStringify` of a literal type (also as a struct field or a
  union member) write its value: strings raw at the top level of `console.log`, quoted `'a'` when
  nested, JSON-escaped for JSON; floats in JS number format. Eq glue is `true`, hash glue `0`,
  no drop/clone work.

## Discriminated unions
- No encoding of their own: a union of object types sharing a literal-typed field is an ordinary
  union enum (see "Union types"); the discriminant field is a zero-sized literal field of each
  member, so the member's variant index *is* the tag. Object types `{ a: T }` written in types
  are the same `AdtKind::Anon` defs as object literals of that shape.
- `x.kind == "lit"` is a bool `Match` on `x` with `Variant { args: [Wildcard] }` arms for the
  members with that discriminant; `x.kind` as a value (and any field all members have) is a
  `Match` on `x` reading each member's field (`Variant(v, [Binding b]) => b.field`, a literal
  field as its constant, a non-Copy field as `Intrinsic::Share` of it), typed as the union of the
  field types.

## Union views
- A union whose members are all interfaces with methods sharing an interface (velt_sema
  `union_views.rs`, #1410) can hold a *view*: an interface value of the union's view interface
  (an interface sema makes: the members' nearest shared interface plus every field all members
  have, readonly, typed as the union of their types; one per family of unions, the members'
  interfaces at any arguments, so a generic union's instance and sema's union of the same
  members hold the same payload type). When the program makes a view of a family
  (`w as U`: `Variant { variant: <view>, args: [Upcast(w)] }`), each union def of the family
  has one more variant, named `<view>`, after the members' (its index is the member count).
- Every match sema makes on such a union has an arm for `<view>` that does the same through the
  view interface (a field read calls the getter, converted to the type the members flow
  narrowing left give it with a checked conversion: `match` on the value or `checked_present`,
  a `TypeError` panic on a mismatch; a discriminant test compares the live value); a `switch` on
  the discriminant matches the discriminant's value (read through the view for a view), its
  slots still narrowing the union local. A union local narrowed to one member that is the object
  of a member read or call, or read where a union of its family is expected, stays the union
  (narrowed) instead of an `UnwrapVariant`, unless it holds that member because it was assigned
  to it.
- Lowering: `canon_union` leaves the `<view>` variant out of a union's member list (the family's
  unions all have it), and a match on such a union that has no arm for the variant panics there
  (`TypeError`) instead of being unreachable. Sema reports a match without a `<view>` arm and a
  narrowed `UnwrapVariant` read of such a union as not supported. Only the families an earlier
  round of checking made views of get `<view>` arms: a round that makes a view of another family
  is checked again with it, so a program that makes no view gets the HIR it got before views.

## switch
- No HIR statement: sema desugars `switch` (`hir/mod.rs`, sema `body/switch`) into
  - a `Match` in statement position on the scrutinee (a place; a temporary is first stored in a
    `Let`): one arm per group of labels sharing a body (`Or` patterns), the `default` group last
    as `_`; arms are `Block` expressions of type `Unit` (or `Never` when the body always
    returns / throws and the match is exhaustive). Case values are `Lit` patterns (inside `Some`
    on `T | null`, `None` for `case null`), `Variant { args: [Wildcard] }` (or `[]` for enums)
    for discriminants / `typeof` tags / literal members / enum members, and `Binding` + guard
    `binding == value` for other expressions;
  - or, when a non-empty body falls through into another, `let <case>: i64 = match (x) { case_i
    => i, _ => default index or n }` followed by `if (<case> <= i) { body_i }` per case (the last
    body unconditionally when the switch is exhaustive);
  - wrapped in `While { label: Some(l), cond: true, body: [dispatch..., Break(Some(l))] }` when
    a `break` targets the switch (`l` is the source label or a synthesized `<switch#n>`); a
    `continue` inside it targeting an enclosing loop is `Continue(Some(label))` (the loop gets a
    synthesized `<loop#n>` label when it has none).
- Case bodies read a narrowed scrutinee local through `UnwrapVariant` / `UnwrapSome` as in any
  flow narrowing; a local narrowed to no member (e.g. in the `default` of an exhaustive switch)
  reads as a `Never`-typed `panic` call.
- Lowering: a `Match` whose arms have no guards and select only by tag / integer
  (`Variant { args: [Wildcard | Binding] }`, `Lit::Int`, `Or` of those; a final `_` / binding
  arm is the default) dispatches through one VIR `Switch` terminator on the tag (`u32` field 0 of
  a tagged enum), the discriminant of a numeric enum, or the integer.

## Enums
- `Def::Enum` without payloads: numeric enums (`VariantDef::discriminant` as written or
  auto-incremented) and string enums (`VariantDef::str_value: Some(s)`, discriminant = member
  index). Both are represented by their `I64` discriminant. A string enum prints, formats and
  `JSON.stringify`s as its string; converting it to `string` is a `Match` yielding the strings.
  Payload variants exist only in compiler-generated enums (unions).
- `PatKind::Range` no longer exists (no user patterns produce it).

## Errors
- `FnDef::throws` is the function's error type: one type, or a union (canonical, see "Union
  types") when several can be thrown. It may mention type params (`function run<E>(f: () => T
  throws E): T throws E`); after substitution, `Some(Never)` — or a union whose members are all
  `Never` — means the instance does not throw. A throwing function returns `Result<ret, E>` at
  the VIR level (`TyKind::Result` is lowering-internal: sema never produces it). Calls of
  throwing functions are ordinary `Call`s: lowering checks the result and branches to the
  innermost enclosing `Try` handler, or propagates (sema guarantees the enclosing fn then throws
  a superset). `throw e` is `ExprKind::Throw`.
- **Widening**: an error reaching a handler (a `Try`'s catch local, the function's own error
  type) has the thrower's type — `throw e`'s operand type, the callee's `throws` (substituted),
  the `FnPtr::throws` of an indirect call, a promise's `E` at `await` — which is the handler's
  type, a member of it, a union whose members are all members of it (re-tag), or a subclass of
  a (member) class (a no-op upcast). Unions built by substitution may nest (`E | A` with
  `E = B | C`); members are searched recursively.
- `StmtKind::Try` catch local: its type is the union of what the body throws (`Never` when
  nothing can be thrown: the handler is dead code). A binding-less `catch {}` gets a synthesized
  local (named `<caught>`) when something can be caught, so a `None` local means nothing is.
- Function values: `TyKind::FnPtr { throws }` is the error type of calling the value (`Never`:
  it does not throw); `Callee::Indirect` calls use the Result ABI accordingly. A closure's
  `FnDef::throws` equals its `FnPtr::throws` (sema fixes it when the closure is created). A named
  function's `FnRef` type may allow more than the def throws: its function-value entry converts
  (wraps `Ok`, widens errors). For an async function type the call does not throw: `throws` is
  `Never` and the error is the result promise's `E`.
- Dispatch groups share one error type: every method in an interface slot (its default and all
  implementations) and in a vtable slot (the base method and all overrides) has the same
  `FnDef::throws`, with no type params, so `Virtual` / `ParamMethod` calls use any member's —
  except in a promise slot (`InterfaceMethodDef::promise`: the interface method returns a
  promise that carries the group's errors): its members are async, or synchronous with no
  `throws`, and a call through the slot never throws. A `Callee::Dyn` call throws
  `InterfaceMethodDef::throws` (in the interface's type params) substituted with the `Dyn`'s
  type args. That type may mention the interface's params when the interface method's `throws`
  clause does (`next(): IteratorResult<T> throws E` in `Iterator<T, E>`); each member's
  `FnDef::throws` is then that clause with the interface args of its implementation
  (`implements Iterator<string, IoError>`: `IoError`), so the members of one slot agree per
  interface instantiation, which is all one vtable holds. A generator method (`is_generator`)
  is no member: its call never throws, and its `FnDef::throws` is its own `E` (the `E` of
  `ret`), so a slot's members may be generators with different `E`s (`[Symbol.asyncIterator]()`
  of `AsyncIterable<T, E>`, whose result type carries `E`).
- Promises: `TyKind::Promise(T, E)` resolves to `T` or rejects with `E` (`Never`: cannot reject).
  An async fn's call has type `Promise<ret, throws>`; `await` of a direct call checks the child
  state's `Result<T, E>` (result region at offset 0), and a promise *value* (heap future) holds
  its result at `+16` as `Result<T, E>` when it can reject, else `T` — also for spawned tasks'
  join handles and the value form of `Promise.all` (which settles to `Result<T[], E>`: the first
  rejection, as soon as it happens). A `spawn(...)` in statement position (its handle is dropped
  at once) reports a rejection as uncaught.
- `Intrinsic::Attempt` (`attempt(f)`): see `hir/intrinsic.rs`; lowering calls `f` with the Result ABI and
  converts `Ok(v)` / `Err(e)` into the call's type by widening (`T | E`), or `null` / the error
  for `E | null`.
- Uncaught errors in `main` (or a detached task) print `Uncaught <Type>` (+ `: <message>` if the
  type has a `message: string` field; for a union, of the member it holds) to stderr and exit 1.

## Sharing (semantics stage 2, docs/internals/design/semantics-stage2.md)
- `Intrinsic::Share(place)` (place borrowed, result owned, same type): another reference to the
  same value — what JS does when a value is assigned, passed on or stored again. Sema emits it
  wherever a non-Copy place of a *shared value* (`Ctx::is_shared_value`: everything that is not
  Copy and holds no promise) would be moved but is used again or cannot be moved from (a
  borrowed param, an array element, a class field, a capture, a by-reference `const` whose place
  the block replaces); implicit copies (spread fields, interface field getters, discriminated
  field reads, async-call arguments used again) are shares too. `Intrinsic::Clone` is a deep copy
  (`x.clone()`, and in async closures for their captures, which several threads may read); a
  class's own `clone()` (no params, returns the class) is what a deep copy of it calls.
  `Intrinsic::Transfer(value)` (written in std only, value owned, result owned, same type): the
  value made safe for another thread like a `spawn` argument — moved where nothing else
  references it, deep-copied where it is still shared (std/prelude/promise.vlt settles promises
  with it; sema wraps the values a `Mutex.with` callback stores across the lock in it,
  velt_sema ownership/locked).
  `Intrinsic::NeedsTransfer(value)` (std only, value borrowed and not read): a constant `bool`,
  whether `Transfer` of a value of that type has anything to do (it can reach a counted object).
- `Intrinsic::ArrayMove(dst, d, src, s, n)` and `Intrinsic::ArraySetLen(xs, len)` (std only,
  std/sort/stable.vlt): the bits of `src[s..s + n)` copied into `dst[d..d + n)` of a different
  array (both ranges bounds-checked; nothing dropped, nothing shared), and a length change that
  neither drops nor initializes (growing the capacity when it is smaller). They bypass
  ownership: the std code keeps every element owned by exactly one array, cuts stale copies off
  before an array is dropped, and keeps the elements out of any array a callback could reach
  through an alias while it runs.
  `Intrinsic::ArrayReserve(xs, extra)` (sema's array spreads): room for `extra` more elements,
  the buffer reallocated to exactly `len + extra` when the capacity is smaller (no doubling).
  `Intrinsic::NeedsDrop(value)` (std only, value borrowed and not evaluated): a constant `bool`,
  whether a value of that type owns anything dropping it releases (lowering's drop glue). A bit
  copy of a type that needs no drop is an independent value, so the stable sort copies such
  elements instead of moving them.
  `Intrinsic::MayAlias(value)` (std only, value borrowed and not evaluated): a constant `bool`,
  whether a value of that type borrowed by a call can be reached through another reference while
  the call runs (lowering counts the type or borrows values of it inside counted objects, so
  params of it are never `noalias`). The array callback methods re-check the length after a
  callback only then.
  `Intrinsic::Truthy(x)` (compiler-internal, `x` a place of a type parameter's type,
  borrowed): a `bool`, JS truthiness of `x` (`!x`, `if (x)` in generic code), lowered per
  instantiation to the test sema writes for that type (truthiness.rs): numbers falsy at `0`,
  `-0` and `NaN`, strings at `""`, nullables at `null` or a falsy payload, unions by their
  member, enums at a `0` / `""` member, other values always truthy.
  `Intrinsic::LooseEq(a, b)` (additive, compiler-internal, both operands of one type with a
  type parameter in it, borrowed: `T`, `T | string`, `T | undefined`): a `bool`, `a == b` in
  generic code, lowered per instantiation: `Intrinsic::Same` (`a === b`) where the instantiated
  type holds no nothing, else that or both operands nothings (`null == undefined`); at
  `T = any` / `unknown`, `AnyLooseEq` (JavaScript's coercions: `1 == "1"`). Sema emits it for
  `==` / `!=` (negated) on such operands.
  `Intrinsic::FnCapturesNothing(f)` (std only, `f` a function value, borrowed): a `bool`, lowered
  to `f.env == null`. Only closures without captures and named functions have a null env (a
  program that compares function values gives every closure one), so true means `f` reaches no
  variable of its caller. velt_opt folds it where the function value is known (const_fields
  propagates the env constant into specialized callees); elsewhere it is tested at run time.
  The stable sort sorts a short array of elements that need no drop in place when its
  comparator captures nothing, and through a copy otherwise.
- Lowering's representation (counted objects, boxed arrays/objects, stabilized borrows) is its
  own business (docs/internals/design/semantics-stage2.md §3); it may turn a move out of a part of a
  counted value into a share.
- `Program::counted_objects` (additive): concrete anonymous object types lowering counts from its
  first pass, whether or not the code that shares them is lowered. Sema accepted an assignment
  into such an object whose right-hand side may replace it (`o.inner.v = g()` where `g` assigns
  `o.inner`, #876) because the program shares the type, so the target must be a counted object
  that keeps the object it named before the right-hand side ran.
- `LocalDef::boxed`: a variable living in a counted cell shared by the enclosing function and the
  escaping closures capturing it (set on both the enclosing local and the closures' capture
  locals, transitively). It is never moved from (sema turns such moves into shares or copies);
  its captures (any by-value mode) hold the cell; borrowed captures point into it as usual.
- `AdtDef::assigned`: a field of the object type is assigned somewhere; such a type is shared as
  one counted object, others may be shared by copying their fields.
- `FieldDef::private` (additive): the field is declared `private` (in the type or the base class
  that declares it; interface fields never are). `console.log` / `inspect` leave out private
  fields of zero size (std's `runtime` markers); other `private` fields show, as in Node; `#x`
  fields never do (ES private names).
- `FieldDef::optional` (additive): the field is declared optional (`a?: T`; its type is then
  `T | undefined`, and `T | null | undefined` for `a?: T | null`), in a class, an object type or
  an interface. `JSON.stringify` leaves it out while it is absent or holds `undefined`, as
  JavaScript leaves out a missing property; `console.log` leaves out an absent field (a clear
  presence flag, below) and shows a class's optional field holding `undefined`, as Node does. A
  `null` (a present one of `a?: T | null`, or a `T | null` field that is not optional) is
  written as `null`.
- `AdtDef::private_fields` (additive): some field, own or inherited, is `private` (or `#x`).
  `JSON.parse` cannot build such a type (sema rejects it; lowering's reader treats one as an
  internal error).
- `AdtDef::opaque` (additive): some private field, own or inherited, is declared by a std type
  (a runtime handle or other internal state: `BigInt`, `RegExp`, sockets, database clients).
  Such a type has no JSON form at all: sema rejects writing it, lowering's writer treats one as
  an internal error, and a base class value whose dynamic class is opaque (or holds an opaque
  value) is written as its static class (`json/dynamic.rs`). Other `private` fields are
  written, as in Node.
- `AdtDef::declared_struct` (additive): the type is declared with `struct` (kind `Struct`,
  but not a field-only interface or a struct sema made for an interface literal or a brand).
  `Map` and `Set` keys of such a type compare (`Intrinsic::KeyEq`) and hash
  (`Intrinsic::Hash`) by content, field by field; `===` still compares it by identity.
- `AdtDef::to_string`, `to_json`, `inspect` (additive): a class's `toString()`, `toJSON()` and
  `__inspect()` methods, own or inherited (sema's `hooks.rs`), as functions glue can call:
  instance methods with no type parameters of their own, neither async nor generators, that
  cannot throw, taking only `this` (`to_json` may also take the key, a borrowed `string`:
  `(this, key)`); `to_string` returns `string`. A user method of another accepted shape is
  recorded as its **adapter** `C.<toString>` / `C.<toJSON>`, a synthesized method of that form
  that calls it as JS does: through `Callee::Virtual` when a subclass overrides it, with the
  defaults of its other parameters, and `to_string`'s result through `Intrinsic::ToString` when
  the method returns something else. Sema reports every other shape. Lowering calls them from
  glue with the object as `this` and drops the result: `String(x)` / `${x}` in code sema could
  not resolve (generic code) calls `to_string`; `JSON.stringify` writes what `to_json` returns
  instead of the fields (an opaque class with one has a JSON form), passing the property name,
  the array index as a string or `""` when it takes the key (`Glue::JsonWriteKey`); `console.log`
  prints what `inspect` returns instead of the fields (a string raw, another value after the
  class name). `to_json` and `inspect` are called on the static class of the value (after
  `JSON.stringify`'s dynamic class switch); `to_string` dispatches on the object's class
  through the adapter.
- Modifying through a pattern / `for...of` / by-reference `const` binding is allowed (JS):
  mutation inference counts it against the place the binding points into.
- `==` / `!=` on non-primitive types are `Intrinsic::Same` (JS `===`: objects — class instances,
  arrays, structs, object literals, interface and function values — by identity; `T | null`,
  unions and tuples part by part; `!=` wraps it in `Not`). `Intrinsic::Eq` is structural
  (`__intrinsic_eq`, `deepEqual`, `assertEq`). `Map` and `Set` keys compare with
  `Intrinsic::KeyEq` (`__intrinsic_key_eq`, JS SameValueZero: `Same` with `NaN` equal to itself
  and `0` to `-0`) and hash with `Intrinsic::Hash`, which agrees with it (objects hash their
  identity; a type declared with `struct`, `AdtDef::declared_struct`, compares and hashes by
  content, field by field). `Array.includes` compares with `Intrinsic::IncludesEq` (`Same`, with a
  number `NaN` equal to `NaN`). Structs are never Copy
  (`AdtDef::is_copy` is false for every struct and object type).

## ES private names

A class member declared `#x` keeps the `#` in its name: `FieldDef::name` is `"#x"` (and
`FieldDef::private` is set), a method's name ends in `.#m`. No identifier starts with `#`, so
`#x` and `x` are different members, and a class and its subclass may each have a field `#x`
(two slots with the same name; sema picks the one the code's class declares). Readers act on
the prefix: `console.log` (`glue/format_object.rs`), `JSON.stringify` (`json/write.rs`,
`json/dynamic.rs`), `Object.keys` and spread leave `#` fields out. `#m` methods never get a
vtable slot. `#x in o` reaches HIR as the class test of `o instanceof C` (`PatKind::InstanceOf`).
`AdtDef::private_fields` covers `#` fields too: `JSON.parse` cannot build such a type. std's
exported handle classes (`BigInt`, `RegExp`, `JsonValue`, `Request`, `Response`, `Server`,
`Statement`, generators, `AbortSignal`, `AbortController`, `TaskScope`, `RedisPipeline`) keep
their state in `#` fields. Handle types that are structs (sockets, files, database clients,
`Mutex`) and classes internal to a std module keep `private` fields. `AdtDef::opaque` keeps
every std type with private state, `private` or `#`, out of JSON.

## Module constant tables

`GlobalDef.init` is evaluated at each use of the constant, with one exception that sema and
lowering agree on. A **table** is a `GlobalDef` whose `init` is an `ArrayLit`

- of an array type whose element is `Int`, `Float` or `Bool`,
- with every element a `Lit` (`Int`, `Float`, `Bool`), a `Unary { op: Neg }` of an `Int` or
  `Float` literal, a `Cast` of an element of this list, or a `Global` whose own `init` is one
  (followed for at most 16 constants).

Sema accepts an `ArrayLit` as a whole initializer only in this form (`body::pure_init`,
`table_elem`), and never nested in another initializer. Lowering may store the elements of a
table once as static data (`velt_vir`, `table_const` folds exactly the forms above, with `as`
semantics for the casts) and read them there for three uses whose base is the `Global` itself:
`Index`, the `ArrayLen` intrinsic, and a borrowing `ForOf`. Every other use of the constant
evaluates the initializer to a new array, like any module constant. The header lowering reads
through, `{ data, len, cap: 0 }`, is local to the code of those three reads: it is never passed,
stored, boxed or handed to the runtime, so nothing can write through it or count it. An empty
`ArrayLit` is not stored. Sema also rejects every use that could modify a table, including the
ones it cannot follow (an argument of a call through a function value, or of a call that also
passes one that may modify its arguments; a move out of it, however it is named), so the new
array is never written either. The two lists must stay equal: an element sema accepts and lowering
does not fold would make the table an array built at every read.

## Reading a `shared<T>` (issue #36)

`Call { Intrinsic(SharedRead), [s] }`, typed `T`, is the value inside the `shared<T>` `s` (its
one argument, a `Borrow` place or any expression of that type). Sema emits it only where the
value is read: the object of `Index`, of a `Field` and of the `ArrayLen` / `StrLen`
intrinsics, the source of a `ForOf`, the receiver of a method of `T` that `shared<T>` itself
doesn't have, an expression converted to `T` (`coerce`: an argument, a variable, a field), and
the argument of `console.log`, of an array spread and of `Object.keys` (`FnCx::read_shared`).
`JSON.stringify` writes a `shared<T>` as its `T` (sema `json/mod.rs`, velt_vir `json/write.rs`).
Never for a `T` that holds a `Mutex`, a weak map or ref, a promise, a function or interface
value, a type parameter, or a class with subclasses or with a `clone()` of its own
(`Ctx::frozen_readable`), nor for a `shared<int>` (its atomics). The exclusivity checker treats
it as a borrow of the place of `s` (`Collector::place_of`), so code that may give that place
another handle while the read is live (a `ForOf` body, a closure argument of the same call) is
refused as it is for an array. Sema rejects every write through it (`require_mutable`,
`use_mutably`) and every call that may modify it: a `BorrowMut` argument after inference, an
`Owned` or `Borrow` argument the callee modifies all the same (`FnInfo::modified_params`: a
method returning the `this` it changes, such as `fill`, a parameter it keeps and changes, an
element a loop over it changes, `Ctx::loop_roots`, through aliases, elements of elements and
closures, or a write through what a method of it returns, `st.m.get(k)!.v = 1`; the argument
is a part of the read or an element a method of it returns, `s.m.get(k)!`), a change to a
`for…of` element of it or of what a method of it returns (`Ctx::frozen_elems`) or of a
variable bound to a part of it, also through loops over those and bindings of them
(`ownership/finish.rs`), `subarray()` and `buffer` of a typed array in it
(`FnCx::frozen_buffer_view`), an
argument of a call through a function value or next to one that may modify its arguments
(`ownership/patch.rs`). Other threads read the same value at the same time, so lowering must
not touch a reference count in it either (velt_vir `lower/frozen.rs`). The call is a borrow of
the value in its box, a *frozen* value; where `T` holds no counted part (`Cx::holds_counted`)
it is read through a bitwise copy of its inline part that is never dropped. A frozen value, or
a part of one, is never counted: wherever it would be (consumed by `FnLower::consume`, kept by a
parameter that keeps its argument, shared, captured by a closure that may outlive the call,
passed through a function value, a vtable or an interface) it is a deep copy (`clone_value`)
instead. A counted one passed to a borrowed parameter of a direct call goes to the callee's
*frozen instance* (`Work::Frozen`, symbol suffix `_Z<mask>`), which treats that parameter as
frozen too; a closure literal passed with it gives that instance the closure's code, so the
callback is called directly through its own frozen instance (`frozen::KnownFn`,
`xs.some((x) => …)`). Bindings by reference into a frozen value, `for…of` elements of one and a
map loop's entries that the loop body leaves unchanged (`frozen_map_entry`) are frozen as well.
The box itself is retained for the statement (one atomic count change,
`FnLower::shared_value`, `BoxHold`) where code may run while the value or a part of it is read
in place, and the place of `s` is one such code can give another handle (a variable in a shared
cell, a module `let` some function reassigns, a field or element): the value as a call's
argument or receiver or a loop's source, and a part of it (a field, an element) that is not a
number, since such a part is passed on, looped over or a receiver where it is read. A number
read out of it is copied at once (also where a frozen operand is otherwise not held) and
retains nothing. An index that may write (`may_write`) retains the box whatever the place: it
may give the variable `s` reads another handle itself.

## `RwLock<T>` (issue #708)

`new RwLock<T>(x)` is `Call { Intrinsic(RwLockNew), [x] }` (`x` owned), typed as the prelude's
`RwLock<T>` (`{ lock: u64; value: T }`, like `Mutex<T>`). `rw.write(f)` is `MutexWith` on an
`RwLock` or `shared<RwLock<T>>` receiver, with every rule `with` has. `rw.read(f)` is
`Call { Intrinsic(LockRead), [rw, f] }` with the same shape and rules, and sema reports a closure
literal or named function `f` that modifies its parameter once modes are inferred
(`ownership/locked/read_only.rs`). Lowering takes the lock shared and runs `f` in velt_rt's read
mode (rt_abi_async.md §9.1).

## Module state

A module-level `let`, and a module `const` sema keeps instead of evaluating at each use (an
initializer that is not a constant, a function from a call; #76, #802), is a `GlobalDef` with
`slot: Some(i)`. `Program::module_state` lists the slots' globals in initialization order
(Node's module evaluation order) and gives the state tuple `ty` (element `i` = slot `i`'s
type); it is `None` when no global has a slot, and then nothing below is emitted.

- `ExprKind::Global(d)` of a slot is a place, like a field: read (a copy for Copy types),
  borrowed (`m.set(k, v)`), assigned (`Assign { place: Global(d) }` drops the old value) and
  modified in place. A use that takes the value (`Want::Move`) is wrapped in
  `Intrinsic::Share` by sema; moving a part out of it (`Field { base: Global(d) }` with mode
  `Move`) is a share too (lowering).
- Lowering emits `velt_rt_module_state(&DESC, i) -> ptr` for each access to slot `i` (rt_abi.md
  "Module state") and projects slot `i` of the tuple behind it; `DESC` is
  `{ size, align, init, drop, names }`, `init` the function evaluating every slot's `init` in
  order into the zeroed block (calling `velt_rt_module_ready` after each), `drop` the tuple's
  drop glue, `names` the slots' names. A slot is never read from its initializer's stored
  data (a module constant table's `table_read`). `velt_main` touches the state before calling `main` (sync or async), so
  every initializer runs first, as in Node.
- The root file's slots when its top-level statements are the entry `main` (#1438): slots
  `ModuleState::deferred..` (the root module is evaluated last, so they are the last ones).
  `init` stops before them; `main` holds `Intrinsic::InitSlots(from, to)` statements (no args,
  `void`) before the first statement after each run of them in the source, and lowering calls
  the initializer of slots `from..to` there on the block (`velt_rt_module_block`). The
  initializer that stores the last slot reports it with `u64::MAX`. The descriptor's `rest`
  initializes slots `deferred..` for a block made for swapped-in code (rt_abi.md).

## Symbols

`TyKind::Symbol` is a pointer-sized Copy value (rt_abi.md "Symbols"). A member a symbol names is
an ordinary field, method or accessor whose name is the symbol's key name: `[Symbol.iterator]`
for a well-known symbol, and for the symbol a module constant holds `"\u{FFFF}Symbol(d)"`
(`"\u{FFFF}Symbol(d) #2"`, `#3`, … for further symbols with description `d`;
`velt_sema::property_order::symbol_key_name`). No identifier starts with `[` or U+FFFF, and
sema rejects quoted keys starting with `[Symbol.` or U+FFFF, so no string key reaches such a
member (`"[Symbol(k)]"` is an ordinary key). Readers act on the form
(`velt_sema::property_order::symbol_key`, `is_hidden_key`): `console.log` shows such a field
after the others as `[Symbol(d)]: v`; `JSON.stringify`, `Object.keys` and `JSON.parse` leave it
(and `#` fields) out, whatever keys the text has. Like every member name, a key name reaches
machine symbol names only through `velt_vir::mangle` (escaped to `[A-Za-z0-9_]`). The value of a
module constant initialized with `Symbol(...)` or `Symbol.for("k")` is
`Intrinsic::SymbolStatic(id, desc, described)` (all literals), one read-only record per `id`
(ids from 2^40: the registry's records, listed in `Intrinsic::SymbolRegistry`'s table).

`KEY in o` with such a key is a `match` on `o` (on a union, per member): `true` for a member
whose type declares the key (a field, method or accessor; a class's optional field too),
`false` for one that doesn't, and for an optional field of an object type or interface
(`[KEY]?: T`) a test of the field: its presence flag (`Intrinsic::FieldPresent`) when it has
one (`FieldDef::presence`), else that it does not hold its nothing. `Symbol.iterator in o` and
`Symbol.asyncIterator in o` are `true` or `false` from `o`'s type's methods.

`Intrinsic::TemplateToString` is a template literal part whose type mentions a type parameter:
`ToString`, except that a `symbol` (once substituted, also as the active member of a union or
`T | null`, tested at run time) panics with JS's `TypeError`.

## Types with no HIR form

Intersection types (`A & B`) and indexed access types (`T["k"]`) never reach HIR: sema resolves
an intersection of object types to one ordinary anonymous object type (`intersections.rs`), and
an indexed access to the type of the field it names (a union for several keys,
`utility_types.rs`). Branded types (`string & { __brand: "UserId" }`) are replaced by their
primitive before HIR leaves sema (`brands.rs`, `readonly::erase`).
In a program that uses a container of `any` as one of `unknown` or back (`Record<string, any>`
as a `Record<string, unknown>`, an `Upcast` between them; `Ctx::same_but_any_unknown`), `unknown`
is replaced by `any` everywhere in the same pass, so the two views are one type for lowering;
other programs keep their `unknown`s (lowering treats both alike).

## Deferred types (issue #934, phase 2)
`TyKind::Op(TyOp, args)` appears only in generic definitions (signatures, fields, bodies): a type
alias applied to arguments that mention type parameters, whose body (a conditional, mapped,
indexed access or template literal type) can't be evaluated until they are known
(`TyOp::Alias`), or an intersection with a part that is a type parameter (`T & { $set: … }`,
`TyOp::Intersect`, its arguments the parts in written order, #350). Lowering never lays one out: when its substitution makes the arguments concrete,
it replaces the `Op` by `Program::type_ops[op]` (looked up as substituted, else with canonical
arguments), the reduced type sema computed for every instantiation the program reaches. A missing
entry is an `ICE:`.

`TyOp::ErrSite(n)` (no arguments) exists only while sema checks bodies: the error type of the
function type written without `throws` at site #n (issue #1154), an error-set variable that sema
solves once after every body is checked. Sema replaces each one, and every type holding one, by
its solved type before the program is built (with the readonly twins), so lowering never sees
one; an error type that was only variables solved empty is no error type (`FnDef::throws` is
`None`).

## Spreads and conversions in generic bodies (#350)
- `{ ...t, tag: e }` with a spread source whose fields are known only per instantiation (a type
  parameter, or a deferred intersection or spread) is a `Call` of `Intrinsic::SpreadObject`
  whose type is `TyOp::Spread` over the parts' types. Every argument is a place of an object
  type, bound to a temporary in written order (a spread source that already is a place is
  borrowed as it is): a spread source, or a run of explicit properties as an object literal of
  their own. Sema reduces `TyOp::Spread` per instantiation to the object type of the merged
  fields (public, non-private fields of each part in order; a later part's field overrides an
  earlier one and keeps its position); a part that isn't an anonymous object type, struct or
  class is an error at that instantiation. An optional field over an earlier value of it keeps
  the earlier field's optionality and type: a required earlier field stays `F`, as TypeScript
  types it (an error there when the two types differ). Lowering
  (velt_vir `lower/spread_object.rs`) builds the reduced type, each field shared from the last
  part that has it and is present (a part's optional field with a presence flag gives it only
  while the flag is set, else an earlier part does; one of an object type without a flag
  holding `undefined` is absent; a class's optional field is always present, as an own
  property), its presence flag copied (set for a class field), a required field's value wrapped
  when the field is `F | undefined`; a present `undefined` over a field that can't hold one
  panics with a `TypeError`. A `SpreadObject`
  used directly as another type of the body (`return { ...x, ...patch }` as a `T`) has that
  type instead of an `Upcast` around it, with the requirement `TyOp::Convert` (reduced to the
  target when the spread's fields are the target's, each of its type or that type with
  `undefined`): lowering builds the target type from the parts.
- A value of a generic body used as another type of that body, where TypeScript accepts it and
  the fields are known only per instantiation (`s: T & { tag }` as `T` or as `{ tag }`, `{ ...t }`
  as `T`, `{ ...t, tag }` as `T & { tag }`), is an `Upcast` of it: the same value. Sema records
  the requirement `TyOp::Convert(n)` over `[from, to]` (`n` numbers the conversion, so each is
  reported where it is written; never the type of an expression) as a type the body
  mentions, so `type_level::instances` reduces it at every instantiation: to `to` when both are
  one object type there (the same canonical type, or the same layout); to `to` as well when
  `to` is an object type whose fields `from` has, of the same types and optionality, plus
  others: lowering's `Upcast` of two object types of different layouts is then a new `to`
  holding those fields (shared, as `SpreadObject` builds one), and sema records the copy with
  `crate::object_copies`, which reports it where the program could tell it from the original
  (TypeScript passes the same object). Any other instantiation is reported. Otherwise lowering's
  `Upcast` is the value itself.
- `{} as D` with `D` an object type known only per instantiation (`Pick<T, K>`, `Partial<T>`) is a
  new `Record<string, any>` converted to `any`; its keys are set through `AnySet`, and converting
  it to `D` is an `AnyTo`, which lowering makes, for an `any` holding a `Record<string, any>` and
  an object type `D`, a new `D` of the record's entries (each converted as `AnyTo` converts it; a
  required field whose key the record lacks is a `TypeError` naming it, an optional one is
  absent). `AnyTo` of an `any` holding a string to a string literal type, or a union of them,
  checks the string's content.
- `TyOp::Inline(n)` (additive): a conditional, mapped, indexed access, `keyof` or utility type
  (`Partial`, `Required`, `Readonly`, `Pick`, `Omit`) written in a generic definition, not in an
  alias, whose evaluation needs a type parameter (`{ [K in keyof M]: Topic<M[K]> }` in a
  signature, `T[K]`, `keyof T`, `Partial<T>`). Its arguments are the definition's type parameters
  (`Param(i)`), with `void` for the ones the form doesn't name, so it is known as soon as the
  ones it names are. Forms written alike in one definition are one `n` (a key read's or write's
  requirement, below, is its own). Like `TyOp::Alias` it is reduced per instantiation
  (`Program::type_ops`).
- `x[k]` and `x[k] = v` with `x` of a type parameter `T` and `k` of a key type known only per
  instantiation (a type parameter `K extends keyof T`, or `keyof T`) are `Call`s of
  `Intrinsic::KeyRead(x, k)` (type `T[K]`, the `TyOp::Inline` form the signature writes) and
  `Intrinsic::KeyWrite(x, k, v)` (type `void`; `v: T[K]`, owned), `x` and `k` borrowed places
  (`x` mutably for a write; any expressions, bound to temporaries in order when they are no
  places). An object type written through `KeyWrite` at some instantiation is
  `AdtDef::assigned`. Sema records the requirement that `K` is one key, or keys naming
  required fields (an `Inline` form that is never a value's type, reduced like
  `TyOp::Convert`'s); any other instantiation is an error. Lowering reads (shared) or assigns the
  field the key's string literal type names, as `x.name` does, switching on the key's tag for a
  union of keys. When those fields have different types, `T[K]` is their union: a read wraps
  the field in its member's variant, and a write evaluates the value once and stores the
  value's member into the field when it is the field's type, else stops the program with a
  `TypeError` (deferred-types.md, decision 4).

## Interface intersections and upcasts (#350)

- An intersection with an interface with methods (`I & { errors: unknown }`, `A & B`), and
  `Pick<I, K>` / `Omit<I, K>` of one, is `TyKind::Dyn` of an interface sema makes
  (`velt_sema` `synth_ifaces.rs`), one per form, generic over the type parameters its parts
  mention. Its `InterfaceDef` is an ordinary one: the members of every part (an intersection
  also lists the interface parts as parents), in slot order. Nothing new reaches lowering
  except the conversions below. An object literal implements it as any interface; a class
  implementing every interface part implements it through an `ImplDef` whose slots are the
  methods of those implementations (of a base class's too) and the getters of the class's
  fields, with setters when the interface is written through. For a generic class it is generic
  over the class's parameters when the parts' arguments are; else it is an `ImplDef` for the one
  instance (`generics` 0), whose class methods take the instance's class arguments.
- `Upcast(e)` with `e: Dyn(A, X)` and the result `Dyn(B, Y)`, `A != B` (additive): `B`'s members
  are members of `A` with the same types (a part of an intersection, a `Pick` of it, an
  interface `A` extends). The value is the same data pointer with `B`'s table for the concrete
  type. `Program::upcasts[A]` lists the targets of `A`, in order; `Program::upcast_impls[i]`, for
  each implementation `i` of `A`, the implementation of each target for the same type (sema
  makes one when none exists, its slots the slots of `A`'s implementation for the same members).
  Each table of `A` holds the table of target `k` at slot `n + k`, `n` being the number of
  slots of `A`'s implementations; lowering loads it. Both `A` to `B` and `B` to `A` may be
  recorded: each table holds the other (lowering fills a table's targets in once the table is
  registered). Values of `A` are compared by identity when values of a target are.

## `extend … implements` (issue #1546)
- An `extend<T…> X implements I` block is an ordinary `ImplDef` (`ty` the target pattern, generic
  when the block is; a blanket block's `ty` is `Param(0)`), listed in `Program::extension_impls`
  (additive) with the bounds on its type parameters (`ImplBound`: parameter, interface, args in
  the impl's params). Lowering's impl lookup for `iface<args>` and a concrete type: the first
  impl that is not an extension impl, in `impls` order, that fits the type or else its nearest
  base class (a type's own implementation comes first); else the most specific extension impl
  that fits the type (its pattern matches and its bounds hold, each bound looked up the same
  way), or else its nearest base class: A over B when A's `ty` is an instance of B's and not the
  reverse, or the same pattern where only A has bounds. Sema refuses two extension impls that
  can fit one type where neither is more specific (whatever their interface arguments), so the
  choice is unique; the bounds of extension impls are interfaces, never `any`, `unknown`,
  `object` or `Function`. A block's bound holds
  when the type meets it at all (its own impl, or through a finite chain of extension impls,
  the least answer): per level (the type, then each base class), through each extension impl
  for the bound's interface and arguments that fits, except that an impl without bounds that
  fits decides the level's answer (the most specific such impl: its arguments are the bound's
  or the bound fails) and no base class is looked at. Sema refuses an extension impl with
  bounds that takes precedence over another with other interface arguments (more specific
  than it for a type both fit, or fitting a type the other fits a base class of), so this
  answer is the arguments of the impl the lookup picks. An interface value `Dyn(D, args)`
  meets a bound `iface<bargs>` listed for `D` in `Program::dyn_bounds` (additive; `D` and the
  interfaces it extends without generic methods, args in `D`'s params) when the listed args
  with `args` substituted are `bargs`. Converting an `any` to
  `iface` uses the same lookup for the held value's type.
- A call through a type parameter's bound that only a blanket block gives it (`k.m()` with
  `K extends Comparable<K>` and `extend<T extends Comparable<T>> T implements I`) is a
  `Callee::ParamMethod` of `I`, resolved by that lookup per instantiation.

## Canonical instances and optional fields (docs/internals/design/deferred-types.md, P1 and P2)
- An instance of a generic object type is the object type it spells out: sema substitutes
  through `Ctx::subst`, which re-interns anonymous object types and unions from their
  substituted parts, so `{ a: T0 }<string>` is never left in a type where `{ a: string }` exists.
  Lowering may still meet such instances (it substitutes on its own while monomorphizing); it maps
  them onto `Program::anon_shapes` (the concrete anonymous def of each shape: field names, field
  types, `optional` and `presence` flags, in order; never a def replaced by readonly erasure,
  and `{ d?: string }` and `{ d: string | undefined }`, whose field types are the same, are two
  shapes) and
  `Program::union_shapes` (the concrete union def of each member list as lowering sees it: the
  payloads after readonly erasure but a union view's `<view>`, deduplicated and sorted by type
  id), so one shape is one
  VIR type. Concrete unions that erasure makes one member list (`number | A` and `number | {
  readonly a: number }`, `A` a field-only interface) are the first def of that list in the
  program lowering gets: every type, `Variant`, `UnwrapVariant` and `PatKind::Variant` of the
  others is that def, with its variant numbers. Variants of one union whose payloads erase to
  one type (`(R | number)[] | (number | A)[] | string`) are the first of them: no `Variant`,
  `UnwrapVariant` or `PatKind::Variant` names the others. Concrete anonymous object types whose
  fields erase alike (`{ v: number | A }` and `{ v: number | R }`, once those unions are one) are
  the first def of those fields. A concrete union whose members erase to one type
  (`USD | number`, `USD` a brand of `number`) is that type, its variants still written as
  `Variant`s: a variant of a value whose type is no enum is the whole value. Sema treats
  members that erase to one type as one member when it re-interns an instance (`T | A` at
  `T = { a: number }` is `A`), as lowering deduplicates them.
  A generic union whose members are not all plain types (a union or nullable member) keeps its
  own variants. An optional field `v?: T` of a generic object type at a dynamic `T` (`unknown`,
  `any`) keeps its option (`{ v?: unknown | undefined }`), as the generic def lays it out,
  while a written `v?: unknown` holds the dynamic `undefined` itself.
- `TyTable::intern` never makes `Option(Option(T))`: an option of an option is the inner option
  (`T | null | null` is `T | null`), whatever the outer option's `Nothing` (see "Undefined"). `WrapSome` and `UnwrapSome` of a value whose type is already
  that option, and a `PatKind::Some` on it, are the identity in lowering.
- `FieldDef::hidden` (additive): a field that is not enumerable, a symbol brand
  `Object.defineProperty` adds without `enumerable: true` (`velt_sema::defined_props`, #922).
  It is an ordinary field otherwise; `console.log` (`format_object`) and spreads leave it out, as
  Node leaves out a non-enumerable property. Readers that ignore it show the brand.
- `FieldDef::presence` (additive): every optional field whose type is an option (`a?: T`, read
  as `T | undefined`, and `a?: T | null`, read as `T | null | undefined`), in an object type,
  field-only interface or struct (not a class, whose optional field is an own property holding
  `undefined` until set, and not `a?: any`, which holds its own `undefined`; #930 phase 2). It
  keeps an absent key apart from a present value, also a present `undefined` or `null`: lowering
  stores a `bool` flag after the aggregate's fields (in field order, one per presence field).
  An absent field holds the option's nothing (`undefined`). An `AdtLit` sets the flag, unless the
  field's value is `Intrinsic::FieldAbsent` (`() -> T | undefined`; outside a literal it is
  `undefined`); any write to the field sets it; `Intrinsic::FieldPresent` (`(field) -> bool`, the
  argument a `Field` place of a presence field) reads it. `console.log` leaves out an absent
  field and prints a present `undefined` or `null`; `JSON.stringify` leaves out an absent field
  and one holding `undefined`, and writes a present `null`; `JSON.parse` sets the flag when the
  key is there with a value the field's type holds (a JSON `null` for an `a?: T` field without
  `null` leaves it absent, unless the reader has flag 2, `JSON.parse(text) as T`: then it fails).
  A spread copies a present field over an earlier value, also one holding `undefined`, unless
  the target field is required and cannot hold `undefined`: then an absent field keeps the
  earlier value and a present `undefined` panics with a `TypeError` message. Copy, clone and equality glue include the flag.

## Undefined (issue #930, phase 1)
- `TyKind::Option(T, Nothing)`: `Nothing::Null` is `T | null` (every option before #930),
  `Nothing::Undefined` is `T | undefined`, and `Nothing::Both` is `T | null | undefined`. A
  union `A | B | undefined` is `Option(union, Undefined)`. `undefined` alone as a type is
  `Option(never, Undefined)` (its one value is the nothing).
- `T | null | undefined` (phase 1b) is an option of a `T | null` whose own nothing is
  `undefined`: its payload (`TyTable::payload`, what `WrapSome`, `UnwrapSome` and
  `PatKind::Some` hold) is `Option(T, Null)`, which `TyTable::intern` interns with it. A present
  value is `WrapSome(WrapSome(v))`, `null` is `WrapSome(Lit::Null)` (the `Lit::Null` typed
  `T | null`), and `undefined` is `Lit::Null` typed `T | null | undefined`; `PatKind::None`
  matches `undefined` only, `Some(None)` matches `null`. Lowering sees the type as
  `Option(Option(T, Null), Undefined)` (`Cx::kind`), so layout and glue are those of two nested
  options. `TyTable::intern` makes `Option(Option(T, Null), Undefined)` (and `Both` over
  `Null`) `Option(T, Both)`, so a generic `U | undefined` at `U = T | null` is that type and
  its `WrapSome` / `Lit::Null` mean the outer option's. Sema converts a `T | undefined` to a
  `T | null | undefined` with a `Match` (the two keep `undefined` in different places).
- `T | undefined` has the layout of `T | null` (the nothing is the null marker: flag `0`, the
  null pointer, `false`); only what observes the nothing differs: `console.log`, template
  strings and `String(x)` write `undefined`, `typeof` gives `"undefined"`, and `JSON.stringify`
  leaves out an object field holding `undefined` (also a non-optional one; a presence flag is
  not consulted) and writes an array element as `null`. Lowering keys glue for
  `T | undefined` apart from `T | null` (`OptionU<T>`).
- `hir::Lit::Null` is the nothing of its option type: `undefined` when that type is
  `T | undefined`. `void e` is a `Block` of `StmtKind::Expr(e)` and that literal; as a
  statement, `void e;` is `StmtKind::Expr(e)`.
- An option of an option otherwise keeps the inner option's `Nothing`: generic code's `U | null` with
  `U = T | undefined` is `T | undefined`, so its nothing is JavaScript's `undefined` (`pop`,
  `?.`). Generic code that keeps a `U | null` of a parameter given a `T | undefined` could not
  tell its own `null` from the value's `undefined`; sema reports such
  instantiations (`undefined_args`, following calls, closures, methods and the generic types a
  definition uses to a fixpoint); the reverse (`U | undefined` at `U = T | null`) is
  `T | null | undefined` above. As a backstop,
  `velt_vir::lower_checked` reports an error (not an ICE) for an instance whose substitution
  makes a `U | null` of a `U` that allows `undefined`; `lower_with` and `lower` panic (`ICE:`)
  there instead.
- AST (additive): `ast::BinaryOp::StrictEq` / `StrictNotEq` are `===` / `!==`; `Eq` / `NotEq`
  are `==` / `!=` (before #930 both spellings were `Eq` / `NotEq`). `BinaryOp::loose()` maps the
  strict forms to the loose ones. They differ only against `null` / `undefined`: `==` matches
  either nothing, `===` only its own (`x === null` on a `T | undefined` is constant `false`,
  and sema warns about it). HIR keeps one `BinOp::Eq` / `NotEq`.
- Optional parameters and fields (phase 2): the parser writes the type of `p?: T` (a function's,
  an arrow's or a function type's parameter) and of an object type's optional field
  `a?: T` as `T | undefined`, adding a zero-width `undefined` at the end of `T`'s span (a written
  type that already includes `undefined` stays as it is); `a?: T | null` becomes
  `T | null | undefined`. An interface's or class's optional field reads as the same type. An
  optional parameter's default is `undefined` (the nothing of its option type), so a call
  that leaves it out passes `undefined`. `?.` gives `undefined` when its object is either
  nothing: on a `T | null` object the result is `R | undefined`, and `R | null | undefined` for a
  `R | null` rest.

## any and unknown (issue #924, phase 1a)
- `any` and `unknown` are interfaces of the prelude (`std/prelude/z_dynamic.vlt`), so their type is
  `TyKind::Dyn(any | unknown, [])`: an interface value `{ data, vtable }`. Sema adds one
  implementation for every type, `ImplDef { ty: Param(0), generics: 1, iface: any, methods: [] }`,
  whose index is `Program::any_impl` (`Program::unknown` names the `unknown` interface). Converting a value to either type is `ToDyn` with that
  index (for `unknown` too: both share one representation and one table per concrete type).
- Sema unpacks before boxing, so a dynamic value always holds one concrete type: a literal type
  becomes its base, a union each member (`union_to_common`), a `T | null` its payload, and its
  nothing, like the literals `null` and `undefined` written where `any` is expected, the nothing
  of `boolean | null` / `boolean | undefined` (the dynamic `null` / `undefined`).
- The table of `any_impl` for a concrete type is its type descriptor (velt_vir `lower/any.rs`).
  Besides the hidden slots of every interface table it holds, at slot 0, the `typeof` tag (an
  integer: 0 `undefined`, 1 `null`, 2 boolean, 3 number, 4 string, 5 symbol, 6 function, 7
  object), and at slots 1 to 4 the glue `AnyTruthy (data) -> bool`, `AnySame (a, b) -> bool`,
  `AnyNumber (data) -> f64` (booleans, numbers and strings; every type's table when
  `Boxing::any_number` is set, that is when the program has an `AnyToNumber`: `null` 0,
  `undefined` and objects `NaN`, a symbol Node's `TypeError`) and `AnyHash (data) -> u64` (agrees
  with SameValueZero: numbers of every width hash their value as a float). Tables are memoized
  per concrete type, so equal tables mean equal types. `===` / `Intrinsic::Same` and
  `Intrinsic::KeyEq` / `Intrinsic::Hash` on a dynamic type (generic code, `Set` / `Map` keys)
  compare the values held, as `AnyStrictEq` does (keys: numbers by SameValueZero).
- An enum without payloads is boxed as its value (`Match` over its members, each boxing its
  number or string), and an `AnyTo` an enum is a chain of `AnyStrictEq` tests against those
  values (a `TypeError` when none matches). An optional field `q?: any` has the type `any`
  itself (not `any | null`) and its default is the dynamic `undefined`; printing leaves it out
  while it holds `undefined`.
- `Intrinsic::AnyTo` converts out (assignment, argument, return, `as T`; an `unknown` only
  with `as T` or after narrowing), checked at run time: the table must be `T`'s (a class
  without type arguments: the class id is `T` or a subclass; a generic class: that
  instantiation's table; `U | null`: `null` and `undefined` give `null`; a union or literal
  `U` with `| null`: a loose `=== null` test first, then per member as below). A mismatch
  stops the program with `TypeError: the value is not a <T> (it is <found>)`. Its operand is
  owned: a place is wrapped in `Intrinsic::Share` of the place read with `UseMode::Borrow`, so
  the place keeps (and later drops) its value. An optional second operand, a string literal
  (`ExprKind::Lit(Lit::Str(text))`), says where the value comes from: a mismatch stops with
  `TypeError: <text>: the value is not a <T> (it is <found>)` (additive; sema writes it for
  the arguments of an overload signature's adapted callback, `crate::overload_adapter`).
  Lowering reads the text while compiling; it is never evaluated.
- A union with a dynamic member is that member (`any | null`, `any | string` are `any`; an
  `unknown` likewise unless `any` is also a member). An `any | null` / `unknown | null` that
  still arises (a generic `T | null` with `T = any`: `Map.get`, `find`, `Channel.receive`, a
  generic class's field) is collapsed where user code reads it: the payload, or the dynamic
  nothing (`undefined` for a standard library result, else the type's own nothing). `x ?? d`
  on a dynamic `x` is `d` (boxed) when `x` holds `null` or `undefined`.
- `typeof x` is `AnyTypeof`; `typeof x === "number"` (also `"string"`, `"boolean"`) narrows a
  dynamic local with `Fact::Class(l, T)`, read as `AnyTo(Share(x))`; `instanceof C` narrows as
  on any interface value (`Downcast`). Conditions use `AnyTruthy`. `===` / `!==` and `==` / `!=`
  with an operand whose checked type is dynamic (or `any | null`) box the other operand and use
  `AnyStrictEq` / `AnyLooseEq` (JavaScript's algorithms for primitives: numbers by value,
  `null == undefined`, `"1" == 1`, `true == 1`; objects by identity: tables of two static types
  of one object, such as a class and its base, compare the data pointers). `==` of an object
  and a boolean, number or string is the lowering's call of the prelude's `__anyLooseEq` (the
  object's `ToPrimitive` with hint `"default"`, compared loosely), made only in that arm of
  `AnyLooseEq`'s tag switch; a function compares `false` with every primitive.
- Operators on `any` (#924): `Intrinsic::AnyTag (x: borrow any) -> i64` is the `typeof` tag of
  the value held (a load from its table), `Intrinsic::AnyToNumber (x: borrow any) -> f64`
  JavaScript's `ToNumber` of a primitive held (one call of slot 3). Sema writes `a op b` with an
  `any` operand (only `Dynamic::Any`: an `object` or `Function` operand is TS2362) as a block
  binding each operand once, left to right, then `ToNumber` of each: an operand whose tag is
  6 or more goes to the prelude's `__anyObjectNumber`, which takes `ToPrimitive(v, "number")`
  (`__anyToPrimitive`: `[Symbol.toPrimitive](hint)`, else `valueOf` / `toString` in the hint's
  order, keeping a primitive result; the dispatch over the program's classes is sema's, written
  where the prelude calls `__intrinsic_any_to_primitive`). `+` adds numbers when both tags are
  at most 3, else it is `__anyAdd` (an `any`); `<` / `>` / `<=` / `>=` go to `__anyRelational`
  when a tag is 6 or more. `x op= v` and `++` / `--` on an `any` place or member read it once
  and write the boxed result; a typed number place with an `any` value (`n += a`) converts the
  result back with `AnyTo`.
- A dynamic value is never boxed again: `ToDyn` has no dynamic operand but a weak key (below; an `any | null`
  unpacks to the `any` it holds). An interface value converted to `any` is `ToDyn` of the
  interface value; lowering keeps its data pointer and takes the `any` table of the concrete
  type from the interface table. Into a union or literal type, `AnyTo` is applied per member
  behind a test (`AnyTypeof` for primitives, `PatKind::InstanceOf` for classes, `AnyStrictEq`
  for literals), with `Intrinsic::Panic` (`TypeError`) when none passes. `console.log` and the format glue print the
  value the table formats; a string at the top level prints without quotes.
- Member reads and calls (phase 1b of #924): `x.f` / `x["f"]` / `x[k]` with a `string` or
  `any` key is `AnyGet(x, key)`, `x[i]` with a number key `AnyIndex(x, i as f64)`, `x.m(a...)`
  `AnyCall(x, "m", text, a...)` and `x(a...)` `AnyApply(x, text, a...)`, where `x` is borrowed,
  each argument is boxed and moved in, `text` is a string literal naming the callee for a
  `TypeError` (`x.m is not a function`), and the result is an `any`. A call with a spread
  argument passes one argument instead, an `any[]` (the array literal `[a..., ...xs]`, never a
  boxed `any`), whose elements move into the call. An `unknown` receiver is
  an error until narrowed. When a program has any of these, `Program::dyn_members` lists the
  methods and accessors (`DynMember`: name, kind, def, declaring type) of every struct, class and
  object type by name, and every `any` table gets four more slots (velt_vir `lower/any_member.rs`):
  5 `AnyGet (data, key, out) -> u8`, 6 `AnyIndex (data, f64, out) -> u8`, 7 `AnyCall (data, key,
  argv, argc, out) -> u8`, 8 `AnyApply (data, argv, argc, out) -> u8`, returning 0 (the result
  is in `*out`), 1 (not supported yet: a `TypeError` naming #924) or 2 (not a function). Without
  them the tables and the VIR are unchanged. The `AnyCall` slot of a string, an array, a number
  or a boolean dispatches by name to the prelude's `__anyStr_<name>(s, args)`,
  `__anyArr_<name><T>(a, args)`, `__anyNum_<name>(n, args)` and `__anyBool_<name>(b, args)`
  (std/prelude/zzzz_any_methods.vlt; the arguments packed into an `any[]`, the result an `any`),
  comparing only the names the program calls through `any` (`Boxing::any_call_keys`); another
  JavaScript built-in of that kind gives 1, any other name 2.
- Member writes (phase 1b of #924): `x.f = v` / `x["f"] = v` / `x[k] = v` with a `string` or
  `any` key is `AnySet(x, key, v)`, with a number key `AnySetIndex(x, i as f64, v)`, of type
  `void`; `x` is borrowed and `v` boxed and moved in. Used as a value, the assignment is a block
  `{ let t = v; AnySet(x, key, Share(t)); t }`. Both run user code (setters): they are in
  `effects::runs_user_code` and `replaced_holders::implicit::glue_args`. `Program::dyn_members`
  also lists each type's `readonly` fields (`DynMemberKind::ReadonlyField`, `def` the type's own
  `Def::Adt`), which a write refuses. The `any` tables get slots 9 `AnySet (data, key, v) -> u8`
  and 10 `AnySetIndex (data, f64, v) -> u8` (velt_vir `lower/any_set.rs`; the slot owns `*v`),
  returning 0, 1 (not supported yet) or 3 (the fixed shape has no such member: Node would add
  it). A program with a write makes `any` an identity interface in lowering, so an object type
  converted to `any` is one counted object, never a copy. A write through `any` counts as a
  write of the field it names (or, for a computed key, of every field) for `object_copies`.
- `Intrinsic::AnyIsArray(x)` is `Array.isArray(x)` on a dynamic `x`: the class id word of an
  array's or tuple's `any` table is `u64::MAX - 1`. `Array.isArray(x)`, `x instanceof C` of a
  generic class and `typeof x === "function"` (on an `object`; an `unknown` narrows to
  `Function`, below) narrow a dynamic local with `Fact::Class(l, any)` (an `unknown` reads as
  `AnyTo(Share(x))` to `any`; an `any` stays as it is).
- `Intrinsic::AnyHas(x, key)` is `key in x` on an `any` `x` (key `string`, or `any`: a number
  names an element by its string), of type `bool`; `x` is borrowed. Lowering evaluates `key`
  before `x`, as JavaScript does, although `x` is the first argument. It runs user code (a
  proxy's `has` trap): in `effects::runs_user_code` and `replaced_holders::implicit::glue_args`.
  The `any` tables get slot 11 `AnyHas (data, key) -> u8` (velt_vir `lower/any_has.rs`; 0 no,
  1 yes, 2 not supported yet) only when the program has such a test.
- `Intrinsic::AnyDelete(x, key)` (std only, `__intrinsic_any_delete`) is `delete x[key]` with a
  `string` key on an `any` `x` that is not a proxy (borrowed), of type `bool`: a record's entry
  is removed (its `__delete`), an optional field made absent, a key the value does not have as
  its own is `true`, an array's `length` `false`. It changes memory (`effects`,
  `replaced_holders::implicit::glue_args`). The `any` tables get slot 15 `AnyDelete (data,
  key) -> u8` (velt_vir `lower/any_delete.rs`; 0 false, 1 true, 2 not supported yet, a
  `TypeError`) only when the program has such a call.
- `Intrinsic::ProxyArrayCall(x, op)` (std only, `__intrinsic_proxy_array_call`) and
  `Intrinsic::ProxyArrayHole(x, index, op)` (std only, `__intrinsic_proxy_array_hole`), with `x`
  an `any` (borrowed, passed by its data pointer: the target of an array method running on a
  proxy, which the proxy keeps alive, or the array a `Reflect` call reaches) and `index` / `op`
  `i64`, of type `i64`, are calls of `velt_rt_proxy_array_call` and `velt_rt_proxy_array_hole`
  (rt_abi.md "Array methods on a proxy"). They run no user code.
- `Intrinsic::AnyKeys(x)` is `Object.keys(x)` on an `any`, `object` or `Function` `x` (borrowed),
  of type `string[]`: the own enumerable string keys of the value held, in JavaScript's order.
  It sets `Program::dyn_members` (a record's keys are its `__keyNames()`). The `any` tables get
  slot 13 `AnyKeys (data, out: *string[]) -> u8` (velt_vir `lower/any_keys.rs`; 0 written, 1 not
  supported yet) only when the program has such a call; `null` / `undefined` are tested before
  the slot. An object literal spreading an `any` or `object` (`{ ...a }`) is a
  `Record<string, any>` literal whose spread is a call of the prelude's `__spreadAny(r, a)`
  (std/prelude/zzz_adapt.vlt). `JSON.stringify` accepts dynamic types anywhere in its argument's
  type: the writer calls slot 14 `AnyJson (data, buf) -> u8` of the value's table (velt_vir
  `lower/json/any.rs`; present only when the program writes one), after testing the tag for
  `undefined`, a function or a symbol (left out as a member, `null` as an element, a
  `TypeError` at the top).
- Proxy and Reflect (#823): the `Proxy` class (std/proxy.vlt; `#target: any`, `#handler:
  ProxyHandler<any>`) and `Reflect` (static methods), globals that a program loads when a module
  names one of them (std/prelude/global/proxy.vlt); a program that doesn't has no proxies, and
  sema calls the prelude's helpers below, which don't test for one. Sema converts `new
  Proxy(...)` (written outside the standard library; type arguments dropped) to `any` at once
  (`ToDyn`); `AnyTo` converts it to a typed value like any other `any`. The member slots of the
  `Proxy` class's `any` table call its `__get(key, receiver)`, `__set(key, value, receiver)`
  and `__has(key)` (velt_vir `lower/any_proxy.rs`), which run the handler's traps or forward
  to the target; its keys slot (`AnyKeys`) calls `__keys()` (the `ownKeys` and
  `getOwnPropertyDescriptor` traps). `delete o.k` / `delete o[k]` of an `any` or `object` is a
  call of the prelude's `__anyDelete(o, k)` (`__anyDeleteProxy` once `Proxy` is loaded: a proxy's `deleteProperty` trap; another value's
  `Intrinsic::AnyDelete`, std only, through slot 15 `AnyDelete (data, key: *string) -> u8` of
  its table, velt_vir `lower/any_delete.rs`), and the
  versions of the prelude's spread, `Object.hasOwn`, `Object.values` and `Object.entries`
  helpers for `any` (std/prelude/zzz_adapt.vlt) in velt:proxy/ops (`__spreadAnyProxy`, …,
  called instead once `Proxy` is loaded) run a proxy's traps in Node's order. With a symbol constant as
  the key, `o[KEY]`, `o[KEY] = v`, `KEY in o`, `delete o[KEY]` and `Object.hasOwn(o, KEY)` of
  an `any` in a program that has loaded `Proxy` are calls of `__anyGetSym` / `__anySetSym` / `__anyHasSym` /
  `__anyDeleteSym` / `__anyHasOwnSym(o, name, KEY)` (std/proxy/ops.vlt), which
  pass the symbol to a proxy and use the member name `name` on other values; `new Proxy(t, h)`
  with an `h` typed `any` is `__proxyNewAny(t, h)`. In a program whose own code has a `new
  Proxy`, `Object.keys(x)` of a class instance or object `x` is `IsTypedProxy(x) ?
  __proxyKeysOf(x) : Object.keys(x)` (`Intrinsic::IsTypedProxy`, made by sema: a test of the
  pointer's tag bit, `false` for a type that cannot hold a proxy) and a spread `{ ...x }` first calls `__proxySpreadCheck(x)`. The JSON slot of
  the `Proxy` class writes what `__json(receiver)` gives. The prelude's own dynamic operations don't set `Program::dyn_members`; a `new
  Proxy` or a `Reflect.*` call in the program's own code does, and a `Reflect.set` call counts
  as a write of every field for `object_copies` (a `new Proxy` does not: every write reaching
  a target through a proxy starts at a noted write). Lowering recognizes the two classes by
  their qualified names (`std/proxy::Proxy`, `::Reflect`; a program's own class
  named `Proxy` is its own) and numbers them last among class ids, so programs without them
  keep theirs.
- Traps that throw (#1174, stage 1): the function types of `ProxyHandler`'s traps
  (std/proxy.vlt) are sites like the function types a user module writes without `throws`
  (sema `throws/fn_types.rs`): a closure stored into one adds its errors (`Error` classes) to it,
  so a call of a trap in `Proxy.__get` & co. throws them and the `Proxy` methods' error types are
  inferred as any function's. `Intrinsic::AnyGet` / `AnyIndex` (`__get`), `AnySet` /
  `AnySetIndex` (`__set`), `AnyHas` (`__has`) and `AnyKeys` (`__keys`) in a user module or in
  std/proxy (whose `Reflect` functions reach a target that may be a proxy) of a program that has
  loaded `Proxy` are a throw source `ThrowSrc::Call` of that `Proxy` method at the operation
  (sema `any_op`): the operation throws what the method throws, empty when no trap throws. The
  HIR encoding is unchanged. Lowering derives the same error type from the method's final
  `throws` (velt_vir `lower/proxy_throws.rs`): when it is not empty, such an operation in a
  function of a user module or of std/proxy tests whether the value's descriptor is the `Proxy`
  class's table and then calls the method directly, as a call of a throwing function (its error
  goes to the innermost handler or leaves the function, errors.rs); other values take the slot.
  A program whose traps throw nothing lowers to the same VIR as before. Everywhere else a
  `Proxy` method that throws is called from (descriptor slots, including `AnyCall`, typed
  proxies, JSON, printing, the rest of the standard library) its error ends the process with
  Node's `Uncaught …` report (`ScopeKind::Uncaught` around the call): stage 2.
- Typed proxies (#823, velt_vir `lower/proxy_typed.rs`): in a program that lowers a `ToDyn` of
  a `Proxy`, the class hierarchies and object types that `AnyTo` in the program's own code
  converts to, and what values of those types are upcast to (`Upcast`), are *proxiable*
  (`VELT_DEBUG_PROXY=1` lists them). A value of a proxiable type is the object's address, or the
  `Proxy` object's address with bit 0 set: `AnyTo` of an `any` holding a `Proxy` sets the bit,
  boxing clears it again. Field reads, writes and compound assignments on a proxiable type test
  the bit and call the `get` / `set` traps (glue `ProxyGet` / `ProxySet` per field type), counts
  clear the bit, and drop and format glue handle the `Proxy` object. Transfer, many-threads
  check and clone glue of a proxiable type (and of interface values, which may hold one) handle
  the `Proxy` object as an `any` holding it, then set the bit again; `instanceof` reads the
  target's class; weak map keys are the untagged `Proxy` object (values of a proxiable type are
  boxed), and a `WeakRef` keeps the tagged word (rt_abi.md). Sema's update-as-value blocks
  (`{ let tmp = s.f; s.f += 1; tmp }`, `{ s.f += 1; s.f }`) on a proxiable field are lowered
  with one read, and assignment-as-value blocks (`{ s.f = v; s.f }`) with none (the value is
  `v`); a field read on a proxiable type counts as running user code when lowering orders
  operands (`sequence.rs`). Without proxiable types nothing changes. Method calls through a proxy whose handler has a `get` trap
  (velt_vir `lower/proxy_method.rs`): in a program with proxiable types, `AnyGet` of a method
  name on a class object gives the method read as a value, a function value whose code is the
  method's unbound thunk (env null; called on its own it throws a `TypeError`). A method call on
  a proxiable class whose receiver holds such a proxy first runs `__get(name, receiver)` (after
  the receiver, before the other arguments) and checks that the answer is the method about to
  run read as a value (for a virtual call, the instance in the target's vtable entry); the call
  then runs with `this` the proxy. A `super.m()` call (`Program::super_calls`) does not read
  through the proxy, as in JavaScript. An
  async method, or one that owns its receiver, cannot be read as a value yet: a call of one
  through a proxy with a `get` trap is a `TypeError` naming #823. A call of a function-typed field of a proxiable object
  type (`Callee::Indirect` of a `Field`) whose base holds a proxy reads the field with
  `__get` (before the arguments) and calls the answer through its `AnyApply` slot with the
  arguments boxed, its result converted to the field's result type (velt_vir
  `lower/proxy_call.rs`). Object conversions keep a proxy's identity: `AdtLit` has
  `converted` ("Object conversions" below), and a converted literal whose source has a proxiable type gives the
  source's proxy when it holds one (and makes the target type proxiable, as `Upcast` does).
- Views (#1485, velt_vir `lower/any_view.rs`): in a program with typed proxies, `AnyTo` to a
  proxiable object type (not a class) of an `any` holding an object or function of another type
  (and not a proxy) is the tagged result of the prelude's `Proxy.__view(x)`, a `Proxy` without traps whose
  `#view` is set: field reads and writes run its forwarding `__get` / `__set` and check the value
  as the field's type. A tagged value whose `Proxy` is a view converts to `any` as the `any` it
  holds (`__target()`, the view released), so the view is the object to every dynamic
  operation. The conversion gives the object's live view when it has one (the runtime's table of
  views, rt_abi.md "Views", which `__view`'s result enters and a view's drop leaves), so an
  object has at most one view at a time, and `===` and key hashes on a proxiable type compare
  the pointer, as on any object type. A weak key of such a type goes the way of an
  `object` key, by the `any` the value converts to (velt_vir `lower/weak_view.rs`). Object
  patterns, `FieldPresent` and `JSON.stringify` on a proxiable type read a tagged value through
  its traps (`JSON` as the `any` it converts to). The prelude's
  `__proxyKeysOf`, `__proxyHas`, `__proxyHasOwnOf` and `__proxyAny`, which sema calls behind
  `IsTypedProxy`, take such a value (no longer a proxy once in an `any`) as a plain one.
- Array views (#1485, velt_vir `lower/array_view.rs`, std/prelude/zzz_adapt.vlt; additive): the
  array types that `AnyTo` targets in the program's own code (also as a union's array member),
  and the arrays their elements hold, are boxed *view types*. `AnyTo` to a view type of an `any`
  holding an array (or tuple) that is no such value is a box whose header is
  `{ data: view | 1, len: 0, cap: 0 }`, `view` the prelude's `__ArrayView<T>` (`__target`: the
  original as an `any`, `__seen`: a mirror `T[]`). A view's own header has length 0, so an
  `Index` read or target below the header's length, a `for...of` step below it and a `push`
  with room are a plain array's, with no test; past the length (a view always is), for
  `ArrayLen` when the header's length is 0, before a `push` grows the array, and for every other
  access to the contents (`content`), bit 0 of `data` is tested. A view first syncs the mirror
  as far as the access needs (`__arrayViewSyncAt` for an `Index` or a `for...of` step,
  `__arrayViewSyncLen` before a `push` / `pop` / `truncate`, `__arrayViewSync` for the rest: an
  element still `===` the original's is kept, others are converted again; elements are
  converted in order as first reached) and the access runs on the mirror; `ArrayLen` is the
  original's length (`__arrayViewLen`). A loop with such tests runs as a copy without the view
  side (`lower/view_unswitch.rs`): a test of a box in a local the loop does not change (by
  assignment or through its address) is false in the copy, entered after one test before the
  loop (a box never becomes a view); any other test that finds a view, and an `Index` past the
  header's length, go on in the original loop at the same point, except an `Index` of a header
  the copy knows is plain (`lower/view_unswitch_plain.rs`: one a test before found plain, with
  nothing in between that may write memory), where it is a plain array's out-of-bounds error.
  The copy's locals are its own, except those whose address the loop takes (a string buffer, a
  captured `let`), which the copy and the original share. Array intrinsics that change
  the array, and assignments whose target goes through a view type, write the mirror back
  after the change, only the written element for an `Index` target and a swap
  (`__arrayViewFlushAt`), the new tail or length for `push` / `pop` / `truncate`
  (`__arrayViewFlushFrom`), else all of it (`__arrayViewFlush`). A call of
  `std/sort/stable::stableSort` on a view sorts a copy of the mirror instead
  (`__arrayViewSort`); `ArrayMove` / `ArraySetLen` on a view stop with a `TypeError`. The
  runtime's table of array views (rt_abi.md "Views") keeps one live view per (original, view
  type), so two conversions give one box. A view converts to `any` as `__target`, drops by
  leaving the table and releasing the view object, and deep-copies or transfers as a plain
  array of the mirror (a transfer in place leaves the table first). Sema writes a cast
  `xs as U[]` of an `S[]` with comparable element types (or to a union with one array member
  `U[]`) as `AnyTo(U[], xs as any)`, upcast to the union.
- Object conversions (#650, #651; sema `body/expr/object_copy.rs`): an object type converted to
  another object type whose fields it has (in another order, or only some of them) is an
  `AdtLit` of the expected type with `converted: Conversion::Copy`, whose fields each read the
  source place (`Field` of it, possibly under `Share`) or are absent (`null` for an optional
  field the source lacks); a source that is not a place is bound to a `let` first. Every other
  `AdtLit` has `Conversion::No`. Lowering builds the literal, except that when the source's type
  is proxiable (typed proxies, above) and the source holds a proxy, the value is that proxy (a
  new reference, the bit kept): decision 2 of #823. A conversion whose copy the program could
  tell from the original (`object_copies.rs`: a later write, `===`, ...) is a compile error,
  except in a program whose own code constructs a `Proxy` when no `AdtLit` in the program builds
  the source type: then it is `Conversion::ProxyOnly`, and a plain source throws a `TypeError`
  naming #823 instead of being copied (a proxy is still shared).
- Interface adapters (#1153): a value of `Dyn(I, A)` (or of a type implementing `I<A>`, made
  an interface value first) converted to `Dyn(I, B)`, where each of `B` is the same as in `A`
  or dynamic, is `ToDyn` of an `AdtLit` of a hidden struct `<I> adapter` with one field, the
  original interface value (moved in; dropping the adapter drops it), through an `ImplDef` of
  `I<B>` for that struct. Sema makes one struct and impl per `(I, A, B)` the program converts
  (`Ctx::iface_adapters`). Each impl method (`<I> adapter.<m>`, `this` borrowed) calls the same
  slot of the field (`Callee::Dyn`), its arguments converted with `AnyTo` (or, for a function,
  an arrow adapting each call) and its result boxed with `ToDyn` of `Program::any_impl` (or
  adapted in turn). The struct is in `Program::counted_objects`, so a share of the converted
  value shares the adapter and the one original it holds. A parameter of type `unknown[]`,
  `Map<unknown, unknown>` or `Set<unknown>` is passed as a `const` initialized by a call of the
  prelude's `__unknownArrayIn` / `__unknownMapIn` (std/prelude/zzz_adapt.vlt) or
  `__unknownSetIn` (std/collections/set.vlt) on the `unknown` argument, and after the slot call
  (its result kept in a temporary) the matching `…Out` writes the copy back; the helpers use
  `AnyCall` / `AnyIndex` / `AnySetIndex`, and such an adapter sets `Program::dyn_members`.
  Once a field of `I` is written through an interface value (`iface_writes`), the impl gets a
  setter (`<I> adapter.<set f>`, `this` borrowed mutably, the value owned) per field, which
  calls the field's setter slot on the wrapped value with the value converted as an argument
  is. Lowering sees only these ordinary forms, and finds the adapters by their struct (a name
  ending in ` adapter`, one interface field): their tables' class id word says so, and `===`,
  `Set`/`Map` key hashes, `instanceof` and the downcast after it look through them at the
  wrapped value, and a conversion of one to `any`, `unknown` or `object` holds the wrapped
  value instead.
  An `AnyTo` of `Dyn(I, B)` (or `Dyn(I, B) | null`) with a dynamic argument also gets one
  generic adapter, made once every body is checked (`iface_adapt_dynamic`): a struct over one
  type parameter per dynamic argument of `B` (`T0..`), wrapping a `Dyn(I, A)` whose arguments
  are those parameters there and `B`'s elsewhere, with a generic impl of `I<B>`; its slots
  box and check `T0..` as generic code does. Lowering takes a dynamic value holding a type that
  implements `I<A>` (itself or through its nearest implementing base class) through that
  implementation, then the adapter's instance at `A`'s arguments; a generic adapter's struct is
  counted (`counted_objects`) at every instance. A field read through an adapter whose type is
  an array of `I` values at other arguments (`kids: Tree<T>[]`) is a new array of the elements,
  each adapted (`map`), and so is a method's result of such a type; such a field has a setter
  that is never called. Once every body is checked, each read of such a field or method result
  through `I` at a dynamic argument or one naming a type parameter is a compile error unless
  the array is used in place without changing it (an element, `.length`, a test, a method
  that leaves it unchanged and keeps no iterator: `body/expr/iface_field.rs`; not `for…of`),
  and so is a write of the field. An interface whose function types take or give such an
  array has no adapter to other arguments.
- `Program::type_names` names structural types as messages do: a type alias's (`type P = …`)
  and a field-only interface's object type, after `readonly` erasure; a type two names reach
  has none. Lowering's `TypeError`s use it.
- `object` and `Function` (issue #926) are two more interfaces of `z_dynamic.vlt`:
  `TyKind::Dyn(object | Function, [])`, named by `Program::object` / `Program::function` (set
  with `any_impl`). They are dynamic types like `any`: the same representation, `ToDyn` with
  `any_impl`, the same tables, `AnyTo` to convert out, and lowering's `is_dynamic` covers them.
  Sema only lets objects (classes, object types, arrays, tuples, maps, functions, interface
  values) into `object` and functions into `Function`; `find_impl` of either interface answers
  for the type, so `T extends object` is a bound that primitives fail. A `ToDyn` operand may then
  be a type parameter bound by `object` / `Function`; when its instantiation is itself dynamic,
  lowering keeps the value as it is. `x as object` of a type parameter without that bound (or
  of one `| null`) is such a `ToDyn` too; lowering unpacks an option or union instantiation and
  stops with a `TypeError` on a primitive. The one `ToDyn` with a dynamic operand is a weak
  operation's key (`WeakMap.set(k, v)` and the others, `new WeakRef(k)`) written `any` and
  converted to `object` unchecked: a primitive reaches the operation, which behaves as in
  JavaScript. A variable passed there as the key is shared (`Intrinsic::Share`), not moved.
  A call of a `Function` is `AnyApply` (result `any`).
  `AnyTo` from `any` / `unknown` to `object` checks the tag: a boolean, number, string or
  symbol is a `TypeError`. `typeof x === "function"` narrows an `unknown` local with
  `Fact::Class(l, Function)`. Once
  any value converts to `object` / `Function`, lowering converts objects to every dynamic type by
  identity (one counted object, as with a write through `any`), so `Set<object>` keys compare by
  identity whichever dynamic type a value went through. So does a program that compares two `any`s neither of
  which is a literal or a primitive made dynamic (`AnyStrictEq` / `AnyLooseEq`), compares `any`s
  in generic code, or keys a `Set` / `Map` by them (#1256).
- Not yet (each a compile error, phase 1b and 2 of #924): compound assignments and updates of
  members of an `any`, other operators, template literals and `String(x)`, `JSON.parse` into an
  `any`, iteration.

## Object built-ins (issue #925)

- `Intrinsic::SameValue(a, b)` (additive), `(borrow T, borrow T) -> bool`, is JavaScript's
  `Object.is`: `Intrinsic::Same`, except that a float compares by value with `NaN` equal to
  itself and `0` unequal to `-0`, part by part through `T | null`, unions and tuples. Sema emits it
  with both operands of one type, also in generic code; lowering resolves it per instantiation
  (`Glue::SameValue`).
- `Program::null_proto_records` (additive): the program calls `Object.create(null)` (sema's scan
  of the user modules, `null` in any parentheses; sema accepts the call only when the scan found
  it). Such a call is an empty `Record<string, V>` whose map is marked with the top bit of its
  `edits` counter (`__markNullProto`, std/prelude/map.vlt). Only when the flag is set does sema
  test the mark (`in` on a record with a key known only at run time or one `Object.prototype`
  has; `structuredClone` of a record is an error then) and lowering print a marked record as `[Object: null prototype] { … }`;
  other programs are unchanged.

## Functions with properties (issue #921)

No new HIR form. A callable object type (an interface or object type with one call signature
and members, `interface Runner { (): T; stop(): void }`) is an `AdtKind::Anon` object type
whose first field is named `#call` and holds the function value (`TyKind::FnPtr`), followed
by the members (`velt_sema` `callables.rs`). Values are built as ordinary `AdtLit`s; a call
`f()` is `Callee::Indirect` through a `Field` read of `#call`, and a conversion to the plain
function type is that field read (shared). As for an ES private name, the `#` keeps the field
out of `Object.keys`, `JSON.stringify` and printing. Held as `any` or `unknown`, its `typeof` tag
is `function`.

## Classes as values (issue #943)

No new HIR form. A construct signature type (`new (n: number) => A`) is an `AdtKind::Anon`
object type of three hidden fields: `#new` (the signature's function type: a closure that
constructs an instance), `#is` (`(v: unknown) => boolean`, the `instanceof` test) and `#name`
(the class's name) (`velt_sema` `fn_types.rs`). A class used as a value of such a type is a
`Global` of the module state, one per class and signature type, whose initializer is the
`AdtLit` of the three closures (`body/expr/ctor_values.rs`); for a class declared in a function,
and one used at a signature type over type parameters, it is a local instead, bound by a `Let`
at the start of the class's block or of the function's body (`body/expr/ctor_locals.rs`) and
captured by closures like any local. `new C(x)` calls `#new`
(`Callee::Indirect` through a `Field` read) and `v instanceof C` calls `#is`. Held as `any` or
`unknown`, its `typeof` tag is `function` (lowering tells by the first field, `#new`).
