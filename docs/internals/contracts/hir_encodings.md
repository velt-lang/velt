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
  follow the same split.
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
- Errors: see "Errors" below.
- `x ?? d`, `a?.b`, `if (x != null)` narrowing are desugared by sema into `Match` with
  `PatKind::Some/None`; narrowed uses read the payload via `ExprKind::UnwrapSome`.
- Closures share the enclosing function's type-parameter numbering.
- Interface fields are read through synthesized getter methods: sema adds one `Def::Fn` per
  (impl, field) returning the field (Copy → copy, else borrow-as-clone) and appends it to the
  interface's method slots, so `x.field` on a `T extends I` / `Dyn` receiver is a
  `ParamMethod`/`Dyn` call.

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
  `__inspect()` methods, own or inherited (sema's `hooks.rs`): instance methods without
  parameters or type parameters of their own, neither async nor generators, that cannot throw
  (`toString` returns `string`). Lowering calls them from glue with the object as `this` and
  drops the result: `String(x)` / `${x}` in code sema could not resolve (generic code) calls
  `to_string`; `JSON.stringify` writes what `to_json` returns instead of the fields (an
  opaque class with one has a JSON form); `console.log` prints what `inspect` returns instead
  of the fields (a string raw, another value after the class name). Each is called on the
  static class of the value (after `JSON.stringify`'s dynamic class switch).
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

## Types with no HIR form

Intersection types (`A & B`) and indexed access types (`T["k"]`) never reach HIR: sema resolves
an intersection of object types to one ordinary anonymous object type (`intersections.rs`), and
an indexed access to the type of the field it names (a union for several keys,
`utility_types.rs`). Branded types (`string & { __brand: "UserId" }`) are replaced by their
primitive before HIR leaves sema (`brands.rs`, `readonly::erase`).

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
  (`T | null | null` is `T | null`). `WrapSome` and `UnwrapSome` of a value whose type is already
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
