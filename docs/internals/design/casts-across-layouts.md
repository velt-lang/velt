# Casts across layouts: interface downcasts, widened objects, method writes, promise views

Status: approved 2026-11-16 (#379, sigx's "casts across layouts" kit group), with the decisions under "Decisions" below. The order is A1, A2, B, E1, D; A1 is implemented.

## Why

TypeScript allows `x as T` when either type is assignable to the other, and Node never checks
it: the value stays the same object. Velt keeps a value's layout, so some of these casts would
change the value, and today they are refused. After the small shapes of #3050, sigx's kit has
these left:

| shape | sigx site | today |
|---|---|---|
| A: an interface value cast to an extension that adds optional members, then written | renderer.ts 1074 `(ctx as InternalComponentContext).__name = …`, 1233 `.__hmrReload = …`; server index.ts 315 `(rq as { _input?: unknown })._input = …` | `cannot cast … adds __name …` |
| B: a member the interface declares as a method, assigned | renderer.ts 1221 `ctx.update = () => …`, hydrate-component.ts 381 | `no field update … is a method` |
| D: a promise cast to another settled type | app-config.ts 173 `resolvePrincipal(rq) as Promise<P \| null>` | `a promise keeps the type of the value it settles with` |
| E: functions (or arrays of them) cast to another parameter type | app-config.ts 363 `op.policies as ServerPolicy \| readonly ServerPolicy[] \| undefined` | `cannot cast` |

The aim is the rule from #379 for each: Node's behaviour, or a compile error; no silent change.

## A. Checked downcasts between related interface values

**What exists.** A value of an interface type with methods is a pair `(data, vtable)`: the
vtable is the one for the static interface. Related interfaces don't convert to each other
(identity_eq.rs); only lib.dom gets a cast, through `any`. Field-only interfaces are object
types, and #1383 (collect/extended_layouts.rs) lets a base and its optional-only extensions
share one layout.

**Mechanism.** A run-time check, as .NET's `castclass` does with its interface maps. A1 needs no
new table: the conversion of an `any` to an interface (any_iface.rs) already is one. It compares
the `any`'s descriptor with that of each type the program's `any` values may hold, and takes that
type's own implementation of the interface. So `x as J`, where `x: I` and `J` extends `I`, is the
value as an `any`, converted to `J` (identity_eq.rs `related_iface_cast`, the route lib.dom casts
already took):

- the object implements `J`: the value is `(data, its J vtable)`, the same object, as in Node;
- it doesn't: the program panics with `TypeError: the value is not a J (it is an object)`, the
  message of any failed conversion of an `any` to `J`.

There is no new runtime Rust, no change to the vtable layout, and no cost in programs without
such a cast. Where Node goes on and the check would stop the program:

- `J` adds only **optional** members: Node adds them when written. Such a cast stays `cannot cast`
  until A2 widens the objects, so A1 takes only an extension that adds a required member (an
  object lacking it fails in Node too, when the member is used).
- A **union** of extensions or `J | undefined`: there is no run-time test of an `any` for one of
  several interfaces yet (#924), so these stay `cannot cast` outside lib.dom.
- A source that may be **`undefined`**: Node passes it on, and an `any` holding `undefined` is no
  `J`. The note says to test it first.
- `J` at **other type arguments** than `I` (`Source<number> as Labeled<string>`, which `tsc`
  refuses too, or `as Labeled<any>`): no object is both, so it stays `cannot cast`.

An object type that is only shaped like `J` (made as an `I`, never converted to `J`) gets its
implementation of `J` too: each object type implementing an interface the program casts from is
recorded as a value `any` may hold (object_ifaces.rs `downcast_from`).

**Cost:** about 30 instructions (3 ns) per cast on an Apple M-series machine with a few
implementing types, measured on a 10-million-cast loop; the check compares one descriptor per type
the program's `any` values may hold, in order, so it grows with that count.

## A, continued. Widening the objects converted to the base

With the lookup alone, sigx's `ctx` still fails. It is an object literal converted to
`ComponentSetupContext`, and has no `__name`. In Node, writing `__name` adds the property. So:

- For each cast `I → J` in the program where `J` adds only **optional** members to `I`, every
  concrete type the program converts to `I` gets room for `J`'s optional fields: hidden slots,
  absent until written. It then implements `J`, so the check of A1 finds its `J` vtable.
- This is #1383's widening, carried from field-only interfaces to every object type converted to
  an interface with methods. The rules are #1383's:
  - one layout per base family;
  - extensions that disagree on a field's type keep their own layout;
  - absent fields are not printed, serialized or listed.
- **Classes** are not widened (decided below). A class converted to `I` that lacks one of `J`'s optional fields, where the program casts `I` to `J`, is a compile error naming the class and the field. A class that declares the field itself implements `J` as it is.
- An anonymous target (`rq as { _input?: unknown }`) where every member is optional and absent
  from `I` counts as an extension of `I`.
- **Cost:** objects of a widened type are larger by the added slots; nothing else changes.

## B. Setter slots for method members

`ctx.update = () => …` where `I` declares `update(): void`. TypeScript allows it: a method
signature is a writable property.

- When the program writes method member `m` of `I` anywhere, `I`'s vtables get a **setter slot**
  for `m`.
- A concrete type that holds `m` as a field of function type (every object literal does:
  `update: () => {}`) fills the slot with a write of that field. Its method slot already calls
  the field, so the write is seen by every call.
- A **class** with a real method `m` has no field to write. Node would add an own property that
  shadows the prototype's method; Velt has no per-object method table. So the write is refused at
  compile time, with today's message, when any class converted to `I` has `m` as a method. That
  is the only remaining limit; a golden records it.

## D. Promise views

`p as Promise<B>` of a `Promise<A>` (A and B related: `unknown` and `P | null`). In Node it is
the same promise, and an await gives whatever `p` settled with. A Velt promise stores its
settled value with `A`'s layout, so it can't be read as a `B`.

- This follows array views (#1485, lower/array_view.rs). The `Promise<B>` types that some cast in
  the program makes are **view types**, and a value of one may be a **view**: a promise box
  flagged as a view, holding the original promise and the conversion glue `A → B`.
- An `await`, or a `then` callback, of a view type tests the flag:
  - not set: the promise itself;
  - set: awaits the original, then converts the value in place, with no extra microtask. So the
    timing is Node's, and a rejection passes through unchanged.
- The conversion is the checked one `v as B` would be. An `unknown` that holds no `B` panics with
  `type assertion failed`, as any checked assertion does.
- **Identity:** as with array views, a table from `(original, view type)` to the view keeps one
  view per pair, so `===`, `Set` and `Map` see one value.
- Programs without such a cast have no view types.

## E. Casts that are the same type per instantiation

sigx's casts in generic code (`principal<P = unknown>`, policies of `P`) are identities at its
default `P = unknown`, and differ only at other instantiations.

- A new compiler-internal intrinsic, `SameType<T, U>(x: T): U`. Lowering resolves it per
  instantiation: where `T` and `U` are the same type, it is the value as it is (no code).
  Elsewhere it is the conversion the cast needs there:
  - a promise view (D) for promises;
  - for function types, part 2 (below).
- **Contract:** adding `Intrinsic::SameType` changes the `hir` contract
  (`docs/internals/contracts`), and the setter slots change the vtable layout in `vir`.
- **Part 2, function views (not proposed):** a function cast to another parameter type is a
  wrapper that converts each argument. Its identity differs from the original's (`===`), which
  Node would not show. It stays a compile error at instantiations where the types differ, with an
  issue to decide on.

## Order and size

1. **A1:** checked downcasts to extensions adding a required member, with goldens.
2. **A2:** widening the converted object types, and anonymous all-optional targets.
3. **B:** setter slots for method members, and the class refusal.
4. **E1:** the `SameType` intrinsic, identity at equal instantiations.
5. **D:** promise views. The other instantiations of E1's promise casts use them.

Each is a PR with a Node-matched golden that fails on `main`, a golden where the rule stops,
and the kit count. Steps 1–3 clear renderer.ts 1074, 1221 and 1233, hydrate-component.ts 381 and
server index.ts 315 (five errors). Steps 4–5 clear app-config.ts 173, and 363 at sigx's default
`P`.

## Decisions (2026-11-16)

- **A failed downcast** is a `TypeError` panic (`type assertion failed: the value is not a J`), like the other checked assertions, and not a compile error.
- **Widening** covers object types and object literals only, not classes. A class that would need a new field is a compile error naming the class and the field.
- **The contract changes** (vtable layout in `vir`, `Intrinsic::SameType` in `hir`) are accepted. Each PR names the ones it makes.
