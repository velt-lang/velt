# Casts across layouts

Status: partly implemented. Issue: [#379](https://github.com/velt-lang/velt/issues/379).

## Summary

TypeScript allows `x as T` when either type is assignable to the other, and Node never checks
it: the value stays the same object. Velt keeps a value's layout, so some of these casts would
change the value. For each such cast Velt gives Node's behavior or a compile error, never a silent
change of the value:

- an interface value cast to an extension of its interface is checked at run time and stays the
  same object;
- when the extension adds only optional members, the objects converted to the base interface get
  room for them, so the write Node allows works;
- a method member of an interface can be assigned through an interface value;
- a promise cast to another settled type, and a cast that is an identity at some instantiations of
  a generic function, are designed below but not built.

## Motivation

Framework code written for Node uses these casts to attach bookkeeping to objects it receives as
an interface, and to replace callbacks:

```ts
interface InternalComponentContext extends ComponentSetupContext { __name?: string }

(ctx as InternalComponentContext).__name = componentName;
(rq as { _input?: unknown })._input = input;
ctx.update = () => rerender();          // `update(): void` is declared as a method
resolvePrincipal(rq) as Promise<P | null>;
```

Each of these is valid TypeScript and does what it says in Node.

## Design

### Checked downcasts between related interfaces

A value of an interface type with methods is a pair `(data, vtable)`, where the vtable is the one
for the static interface. `x as J`, where `x: I` and `J` extends `I`, is checked at run time:

- the object implements `J`: the result is `(data, its J vtable)`, the same object, as in Node
  (`===` holds, and a write through `J` shows through `I`);
- it does not: the program panics with
  `TypeError: type assertion failed: the value is not a J (it is an object)`, as the other checked
  assertions do.

What implements `J` is what a plain conversion to `J` accepts: a class that declares
`implements J` (itself or through a base class), since Velt's classes are nominal, or an object
type with `J`'s members. An object type that is only shaped like `J` (made as an `I`, never
converted to `J`) gets its implementation of `J` too, because each object type implementing an
interface the program casts from is recorded as a value `any` may hold. An object made as an
interface extending `J` (a `Both extends J, K` value seen as an `I`) implements `J` through that
interface: each such interface is recorded as converting to `J`, which gives every type
implementing it an implementation of `J` (velt_sema `note_extension_upcasts`). A class only
shaped like `J` fails the check, where Node goes on.

`x as J | K` for extensions `J` and `K` of `I`, and `x as J | undefined` (also from an
`I | undefined`, whose `undefined` passes through), are checked the same way: the value converts
to the first member, in order, that its object implements (velt_vir `union_iface`, the glue
`AnyIface` per member, which returns whether it converted), and one implementing none panics with
the `TypeError`. Unions of std's `lib.dom` interfaces convert to the first member unchecked, as no
DOM value exists on a server.

The casts that stay `cannot cast`:

- from a source that may be `undefined` to a target without `undefined`: Node passes `undefined`
  on, and it is no `J`; test it first;
- to `J` at other type arguments than `I` (`Source<number> as Labeled<string>`): no object is
  both.

### Widening the objects converted to the base

When `J` adds only **optional** members to `I`, an object converted to `I` usually lacks them,
and Node adds the property when it is written. So for each such cast in the program, every object
type the program converts to `I` gets `J`'s optional fields after its own, absent until written.
It then implements `J`, and the checked cast finds its implementation.

- The rules are those of extended layouts for field-only interfaces: one layout per base family;
  extensions that disagree on a field's type are an error naming the type and the field; absent
  fields are not printed, serialized or listed.
- An anonymous target (`rq as { _input?: unknown }`) whose members are all optional and absent
  from `I` counts as an extension of `I`.
- **Classes are not widened.** A class's objects have one layout per class. A class converted to
  `I` that lacks one of `J`'s fields makes the cast a compile error naming the class and the
  field; a class that declares the field itself implements `J` as it is.
- An object literal written as `I` whose methods use `this` is laid out as a class and is not
  widened either; the error says to give the literal the type `J` where it is made. So is an
  object literal made in a generic function, which is laid out per instance.

### Assigning method members

`ctx.update = () => …`, where `I` declares `update(): void`, is allowed, as TypeScript allows it:
a method signature is a writable property.

- When the program writes method member `m` through a value of `I`, `I`'s vtables get a **setter
  slot** for `m`, after the field setters.
- An implementation that holds `m` as a field of the method's function type (an object literal
  `{ update: () => {} }`) fills the slot with a write of that field. Its method slot already calls
  the field, so every later call sees the new function.
- An implementation that cannot store it makes the write a compile error naming it: a class with
  a real method `m` (Node would add an own property shadowing the prototype's method; Velt has no
  per-object method table), an object literal whose method `m` uses `this`, and a field of another
  function type.

Nothing changes for an interface never written through: its tables and code stay as they are.

## Compilation

- The checked downcast is the conversion of the value to `any` and from `any` to `J`
  (`Intrinsic::AnyTo`, `crates/velt_sema/src/body/expr/identity_eq.rs`,
  `related_iface_cast`). The conversion from `any` to an interface
  (`crates/velt_vir/src/lower/any_iface.rs`) compares the `any`'s descriptor with that of each
  type the program's `any` values may hold and takes that type's implementation of the interface,
  so it needs no new table and no change to the vtable layout. The object types recorded for the
  check are in `crates/velt_sema/src/object_ifaces.rs` (`downcast_from`).
- Widening is `crates/velt_sema/src/object_widen.rs`, run once every body is checked
  (`object_ifaces::make_dynamic_object_impls`), building on the extended layouts of
  `crates/velt_sema/src/collect/extended_layouts.rs`.
- Each added field is held out of line (`hir::FieldDef::cell`): one pointer slot, `null` while the
  field was never written, pointing to a hidden one-field class that holds the value, made on the
  first write. Reflection (printing, keys, `in`, spreads, `any` reads and writes) sees it as an
  optional field (`crates/velt_vir/src/lower/cell_fields.rs`).
- Setter slots for methods are `crates/velt_sema/src/iface_method_writes.rs`, next to the field
  setters of `crates/velt_sema/src/iface_writes.rs`.

## Runtime cost

Programs without such a cast are unchanged.

- **Checked downcast:** about 30 instructions (3 ns) per cast on an Apple M-series machine with a
  few implementing types, measured on a loop of 10 million casts. The check compares one
  descriptor per type the program's `any` values may hold, in order, so it grows with that count.
- **Widening:** in a program with such a cast, every object of a type converted to `I` is one
  pointer larger per added field, written or not; a written field also costs its cell. Measured on
  an Apple M-series machine (release runtime, 2,000,000 `{ id, run }` literals converted to `Ctx`,
  each read 5 times, one written):

  | | struct | peak RSS | instructions |
  |---|---|---|---|
  | no cast | 24 B | 115.5 MB | 426.8 M |
  | cast, field out of line | 32 B | 130.8 MB | 456.6 M (+7.0%) |
  | control: one more number field, no cast | 32 B | 130.7 MB | 454.5 M |
  | cast, field inline (slot and presence flag) | 40 B | 195.0 MB | 511.8 M |

  The out-of-line field costs what an ordinary 8-byte field costs; of the +7.0%, 2.9 points are
  the checked cast's route through `any`.

## Diagnostics

A cast Velt refuses, with a note saying why:

```text
error: cannot cast `Shape | undefined` as `Named`
a `Shape | undefined` may be `undefined`, which is no `Named`
error: cannot cast `Source<f64>` as `Labeled<string>`
a `Labeled<string>` is not a `Source<f64>` (their type arguments differ)
```

A cast whose widening an object type cannot take, with a note per such type:

```text
error: cannot cast `Ctx` as `InternalCtx`
class `Real` implements `Ctx` and doesn't declare `__name`: a class's objects get no fields it
doesn't declare; declare `__name` on `Real` (optional is fine)
```

A method write an implementation cannot store, with a note per such implementation:

```text
error: cannot assign to method `update` of `Ctx`
class `Real` implements `Ctx` with a method `update`, which its objects share through the class
```

A promise cast to another settled type, until promise views exist:

```text
error: cannot cast `Promise<A>` as `Promise<B>`
a promise keeps the type of the value it settles with, so a `Promise<A>` can't be read as a
`Promise<B>`; convert the value where it is awaited (`(await p) as …`)
```

## Compatibility

Where Node goes on and Velt stops: a class only shaped like the target interface fails the
checked downcast, and a class that lacks an optional field the cast adds, or that declares a
method the program assigns, is a compile error. Everywhere else the cast is the same object, as in
Node.

## Alternatives considered

**A failed downcast as a compile error.** Whether an object implements `J` is known only at run
time for values that flow through interfaces, so a compile-time answer would refuse correct
programs. A `TypeError` panic matches the other checked assertions.

**Widening classes too.** A class's objects share one layout and one vtable per class; giving
every class converted to `I` the extension's fields would grow objects in code that never sees
the cast. Declaring the optional field on the class costs the same and is explicit.

**Holding the added fields inline.** An inline slot with a presence flag costs 40 bytes per object
in the measurement above against 32 out of line, and the field is usually never written.

**Function views for casts between function types.** A function cast to another parameter type
could be a wrapper converting each argument, but its identity would differ from the original's
(`===`), which Node never shows. Such casts stay compile errors at instantiations where the types
differ.

## Limitations

- Casts to a union of interface extensions are refused outside `lib.dom`.
- A class cannot gain fields or per-object methods through a cast or a write.
- A function value cannot be cast to another parameter type.

## Not yet implemented

### Promise views

`p as Promise<B>` of a `Promise<A>` (A and B related, such as `unknown` and `P | null`). In Node
it is the same promise, and an await gives whatever `p` settled with. A Velt promise stores its
settled value with `A`'s layout, so it cannot be read as a `B`.

- As with array views (`crates/velt_vir/src/lower/array_view.rs`), the `Promise<B>` types that a
  cast in the program makes are **view types**, and a value of one may be a **view**: a promise
  box flagged as a view, holding the original promise and the conversion `A → B`.
- An `await`, or a `then` callback, of a view type tests the flag. Not set: the promise itself.
  Set: it awaits the original, then converts the value in place, with no extra microtask, so the
  timing is Node's and a rejection passes through unchanged.
- The conversion is the checked one `v as B` would be: an `unknown` that holds no `B` panics with
  `type assertion failed`.
- As with array views, a table from `(original, view type)` to the view keeps one view per pair,
  so `===`, `Set` and `Map` see one value.
- Programs without such a cast have no view types.

### Casts that are an identity per instantiation

In generic code (`function principal<P = unknown>(…)`) a cast such as `x as Promise<P | null>` is
an identity at the default `P = unknown` and differs only at other instantiations. A
compiler-internal intrinsic, `SameType<T, U>(x: T): U`, is resolved per instantiation in lowering:
where `T` and `U` are the same type it is the value as it is (no code); elsewhere it is the
conversion the cast needs there (a promise view for promises), or a compile error at that
instantiation where no conversion exists (function types). Adding `Intrinsic::SameType` changes
the `hir` contract ([contracts](../contracts/README.md)).
