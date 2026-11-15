# Casts across layouts: interface downcasts, widened objects, method writes, promise views

Status: proposal (#379, sigx's "casts across layouts" kit group). Nothing here is implemented yet.

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

**Mechanism (after .NET).** In .NET every object's type has an interface map, and `castclass`
or `isinst` looks the target interface up in it at run time. Velt does the same behind
TypeScript's syntax:

- Every vtable gets one more word: a pointer to its concrete type's **interface map**. That is
  static data, sorted by interface id, holding `(interface id, vtable)` for every interface the
  type is converted to in the program, or that a cast in the program asks for.
- `x as J`, where `x: I` and `J` extends `I` (or `I` extends `J`), looks up `J` in the map of
  `x`'s vtable:
  - found: the value is `(data, that vtable)`, the same object, as in Node;
  - not found: the program panics with `TypeError: type assertion failed: the value is not a J`,
    like the other checked assertions (narrowing_casts.rs).
- The lookup is a binary search over a few entries, done in generated code over static tables.
  There is no new runtime Rust.
- `x as J` where `J` is a union of related interfaces looks each member up in order.
- **Programs without such a cast** get no maps and no extra word; the pass is skipped.

## A, continued. Widening the objects converted to the base

With the lookup alone, sigx's `ctx` still fails. It is an object literal converted to
`ComponentSetupContext`, and has no `__name`. In Node, writing `__name` adds the property. So:

- For each cast `I → J` in the program where `J` adds only **optional** members to `I`, every
  concrete type the program converts to `I` gets room for `J`'s optional fields: hidden slots,
  absent until written. It then implements `J`, so its interface map has a `J` vtable.
- This is #1383's widening, carried from field-only interfaces to every object type converted to
  an interface with methods. The rules are #1383's:
  - one layout per base family;
  - extensions that disagree on a field's type keep their own layout;
  - absent fields are not printed, serialized or listed.
- **Classes** converted to `I` get the same hidden slots, as Node's instances take new
  properties. A class that declares a field of that name with another type keeps its layout, and
  its downcast fails at run time as above.
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
  (`docs/internals/contracts`), and the interface maps and setter slots change the vtable layout
  in `vir`.
- **Part 2, function views (not proposed):** a function cast to another parameter type is a
  wrapper that converts each argument. Its identity differs from the original's (`===`), which
  Node would not show. It stays a compile error at instantiations where the types differ, with an
  issue to decide on.

## Order and size

1. **A1:** interface maps and checked downcasts, with goldens. Also lib.dom casts, which can then
   stop going through `any`.
2. **A2:** widening the converted object types, and anonymous all-optional targets.
3. **B:** setter slots for method members, and the class refusal.
4. **E1:** the `SameType` intrinsic, identity at equal instantiations.
5. **D:** promise views. The other instantiations of E1's promise casts use them.

Each is a PR with a Node-matched golden that fails on `main`, a golden where the rule stops,
and the kit count. Steps 1–3 clear renderer.ts 1074, 1221 and 1233, hydrate-component.ts 381 and
server index.ts 315 (five errors). Steps 4–5 clear app-config.ts 173, and 363 at sigx's default
`P`.

## Open questions

- Should the downcast's run-time failure be a `TypeError` panic (as other checked assertions) or a
  compile error where the program converts to `I` a type that can't implement `J`? This proposal
  takes the panic, which matches the other assertions; the compile error would be stricter than
  tsc.
- Should widening reach classes (hidden slots in every instance) or stop at object types and
  object literals? This proposal includes classes, since Node's instances take new properties.
