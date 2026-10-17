# HIR encodings (contract detail for `crates/velt_sema/src/hir/`)

How sema encodes language features in HIR; lowering relies on every point here.
Maintainer-owned, like `hir/mod.rs` and `hir/intrinsic.rs`.

## M2 additions
(see docs/reference/classes.md and docs/reference/memory.md):
- Class instances are heap-allocated and referenced by a pointer; `Option<Class>` uses null.
  `new C(args)` is `ExprKind::New`: lowering allocates and calls the constructor (a `Def::Fn`
  whose first param is `this` with `PassMode::BorrowMut`). `super(args)` in a constructor is a
  root-level statement of its body: a `Call` of the base constructor with `Upcast(this)`, or
  `Lit(Unit)` when no ancestor has a constructor. Field defaults (`FieldDef::default`) run
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

## M3/M4 encodings (lowering assumptions, confirmed at merge of ir-3)
- An async fn's `FnDef::ret` is the body's return type `T` (the call's type is `Promise<T>`).
- `spawn` accepts a promise value or an async closure. Promise errors: see "Errors" below.
- `Mutex<T>` is laid out as `struct { lock: u64; value: T }`. `Mutex<scalar>.with` gets a copy.
- `JsonError` and `JsonValue` are resolved by name in the prelude. Optional fields are `T | null`
  fields with a `null` default.
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
- In the body: `Call { Intrinsic(Yield), [v] }` (`v: T` owned, type `Unit`) is `yield v`;
  `Return(None)` ends the generator (no `Return(Some)` occurs); `yield* src` has no form of its
  own (a `Block` expression holding the desugared `for...of` that yields each value). No
  `finally` block of a generator contains a `Yield`, can throw, or contains a `Break` /
  `Continue` whose target loop is outside it (sema).
- The `Generator` class's methods (prelude) use `Intrinsic::GeneratorResume` (`bool`, throws
  `E`), `GeneratorValue` (`T`, moved out; only after a resume returned true) and
  `GeneratorReturn` (close), each taking the `Generator<T, E>` operand `BorrowMut`.
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
- `TyKind::FnPtr` has no mutability information: `{ params, ret }`.
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
  `T | null`), in a class, an object type or an interface. `JSON.stringify` and `console.log`
  leave it out while it is absent (`null`, or a clear presence flag, below), as JavaScript leaves
  out a missing property; a `T | null` field that is not optional is written as `null`.
- `AdtDef::private_fields` (additive): some field, own or inherited, is `private` (or `#x`).
  `JSON.parse` cannot build such a type (sema rejects it; lowering's reader treats one as an
  internal error).
- `AdtDef::opaque` (additive): some private field, own or inherited, is declared by a std type
  (a runtime handle or other internal state: `BigInt`, `RegExp`, sockets, database clients).
  Such a type has no JSON form at all: sema rejects writing it, lowering's writer treats one as
  an internal error, and a base class value whose dynamic class is opaque (or holds an opaque
  value) is written as its static class (`json/dynamic.rs`). Other `private` fields are
  written, as in Node.
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
  (`__intrinsic_eq`, `deepEqual`, `assertEq`, `Map` keys). Structs are never Copy
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
(`[KEY]?: T`) a test of the field: its presence flag (`Intrinsic::FieldPresent`) for
`[KEY]?: T | null` in an object type, else not null. `Symbol.iterator in o` and
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

## Deferred types (issue #934, phase 2)
`TyKind::Op(TyOp, args)` appears only in generic definitions (signatures, fields, bodies): a type
alias applied to arguments that mention type parameters, whose body (a conditional, mapped,
indexed access or template literal type) can't be evaluated until they are known
(`TyOp::Alias`), or an intersection with a part that is a type parameter (`T & { $set: … }`,
`TyOp::Intersect`, its arguments the parts in written order, #350). Lowering never lays one out: when its substitution makes the arguments concrete,
it replaces the `Op` by `Program::type_ops[op]` (looked up as substituted, else with canonical
arguments), the reduced type sema computed for every instantiation the program reaches. A missing
entry is an `ICE:`.

## Canonical instances and optional fields (docs/internals/design/deferred-types.md, P1 and P2)
- An instance of a generic object type is the object type it spells out: sema substitutes
  through `Ctx::subst`, which re-interns anonymous object types and unions from their
  substituted parts, so `{ a: T0 }<string>` is never left in a type where `{ a: string }` exists.
  Lowering may still meet such instances (it substitutes on its own while monomorphizing); it maps
  them onto `Program::anon_shapes` (the concrete anonymous def of each shape: field names, field
  types, `optional` and `presence` flags, in order; never a def replaced by readonly erasure,
  and `{ d?: string }` and `{ d?: string | null }`, whose field types are the same, are two
  shapes) and
  `Program::union_shapes` (the concrete union def of each sorted member list), so one shape is one
  VIR type. A generic union whose members are not all plain types (a union or nullable member)
  keeps its own variants.
- `TyTable::intern` never makes `Option(Option(T))`: an option of an option is the inner option
  (`T | null | null` is `T | null`), whatever the outer option's `Nothing` (see "Undefined"). `WrapSome` and `UnwrapSome` of a value whose type is already
  that option, and a `PatKind::Some` on it, are the identity in lowering.
- `FieldDef::presence` (additive): an optional field whose declared type is nullable
  (`a?: T | null`), in an object type or struct (not a class). It keeps an absent key apart from a
  present `null`: lowering stores a `bool` flag after the aggregate's fields (in field order, one
  per presence field). An `AdtLit` sets it, unless the field's value is
  `Intrinsic::FieldAbsent` (`() -> T | null`; outside a literal it is `null`); any write to the
  field sets it; `Intrinsic::FieldPresent` (`(field) -> bool`, the argument a `Field` place of a
  presence field) reads it. `JSON.stringify` and `console.log` write a present `null` and leave
  out an absent field, and `JSON.parse` sets the flag when the key is there. Copy, clone and
  equality glue include the flag.

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
  either nothing, `===` only its own (`x === null` on a `T | undefined` is constant `false`).
  HIR keeps one `BinOp::Eq` / `NotEq`.

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
  `AnyNumber (data) -> f64` (booleans, numbers and strings) and `AnyHash (data) -> u64` (agrees
  with SameValueZero: numbers of every width hash their value as a float). Tables are memoized
  per concrete type, so equal tables mean equal types. `===` / `Intrinsic::Same` and
  `Intrinsic::Eq` / `Intrinsic::Hash` on a dynamic type (generic code, `Set` / `Map` keys)
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
  the place keeps (and later drops) its value.
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
  of one object, such as a class and its base, compare the data pointers; `==` of an object
  and a primitive stops with a `TypeError`).
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
  `TypeError` (`x.m is not a function`), and the result is an `any`. An `unknown` receiver is
  an error until narrowed. When a program has any of these, `Program::dyn_members` lists the
  methods and accessors (`DynMember`: name, kind, def, declaring type) of every struct, class and
  object type by name, and every `any` table gets four more slots (velt_vir `lower/any_member.rs`):
  5 `AnyGet (data, key, out) -> u8`, 6 `AnyIndex (data, f64, out) -> u8`, 7 `AnyCall (data, key,
  argv, argc, out) -> u8`, 8 `AnyApply (data, argv, argc, out) -> u8`, returning 0 (the result
  is in `*out`), 1 (not supported yet: a `TypeError` naming #924) or 2 (not a function). Without
  them the tables and the VIR are unchanged.
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
- Proxy and Reflect (#823): the prelude's `Proxy` class (std/prelude/zz_proxy.vlt; `#target:
  any`, `#handler: ProxyHandler<any>`) and `Reflect` (static methods). Sema converts `new
  Proxy(...)` (written outside the standard library; type arguments dropped) to `any` at once
  (`ToDyn`); `AnyTo` converts it to a typed value like any other `any`. The member slots of the
  `Proxy` class's `any` table call its `__get(key, receiver)`, `__set(key, value, receiver)`
  and `__has(key)` (velt_vir `lower/any_proxy.rs`), which run the handler's traps or forward
  to the target. The prelude's own dynamic operations don't set `Program::dyn_members`; a `new
  Proxy` or a `Reflect.*` call in the program's own code does, and a `Reflect.set` call counts
  as a write of every field for `object_copies` (a `new Proxy` does not: every write reaching
  a target through a proxy starts at a noted write). Lowering recognizes the two classes by
  their qualified names (`std/prelude/zz_proxy::Proxy`, `::Reflect`; a program's own class
  named `Proxy` is its own) and numbers them last among class ids, so programs without them
  keep theirs.
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
  then runs with `this` the proxy. A direct call of a method its receiver's class overrides
  (`super.m()`, `this` passed upcast) does not read through the proxy, as in JavaScript. An
  async method, or one that owns its receiver, cannot be read as a value yet: a call of one
  through a proxy with a `get` trap is a `TypeError` naming #823. A call of a function-typed field of a proxiable object
  type (`Callee::Indirect` of a `Field`) whose base holds a proxy reads the field with
  `__get` (before the arguments) and calls the answer through its `AnyApply` slot with the
  arguments boxed, its result converted to the field's result type (velt_vir
  `lower/proxy_call.rs`). Object conversions keep a proxy's identity: `AdtLit` has
  `converted` ("Object conversions" below), and a converted literal whose source has a proxiable type gives the
  source's proxy when it holds one (and makes the target type proxiable, as `Upcast` does).
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
  Lowering sees only these ordinary forms.
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
  identity whichever dynamic type a value went through.
- Not yet (each a compile error, phase 1b and 2 of #924): compound assignments and updates of
  members of an `any`, other operators, template literals and `String(x)`, `JSON.stringify` of
  an `any`, iteration.
