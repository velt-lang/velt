# Typed causes of render errors

Status: proposed. Issue: [#82](https://github.com/velt-lang/velt/issues/82).

## Summary

`velt/jsx` reports a failing component as a `RenderError` that holds the original error only as
text. This note gives `RenderError` a `cause: Error | null` field, as ES2022's `Error.cause`, so a
page can narrow it with `instanceof` and react to what the component threw. It needs one
std-internal intrinsic that turns a caught value of a generic error type into an `Error`.

## Motivation

A component that throws, or an async component whose promise rejects, makes `renderToString` and
`renderToStringSync` throw a `RenderError { component; message }` (`std/jsx/errors.velt`);
`renderToStream` prints it to stderr and cuts the response off. The original error survives only
as text (`message` embeds `${e}`), so a page cannot react to what went wrong:

```ts ignore
try {
  html = await renderToString(page(id));
} catch (e) {
  // e: RenderError. Was it the `NotFound` that `Post` threw? Only its text says so.
}
```

`JSX.Element` is not generic over error types, so the type of a component's error cannot travel
with the element to the call that renders it.

## Design

`RenderError` gains `cause: Error | null`; `message` and `component` stay as they are. `cause` is
the original error when it is an instance of a class extending the prelude `Error` (what Velt code
throws almost always), and `null` for other thrown values, whose text stays in `message`. The page
narrows it with `instanceof`, which works on an `Error | null` field:

```ts ignore
try {
  html = await renderToString(page(id));
} catch (e) {
  const cause = e.cause;
  if (cause instanceof NotFound) {
    return notFoundPage(cause.id); // the component's own error, its fields included
  }
  throw e;
}
```

The static type of what rendering throws stays `RenderError`, so every caller's `throws` clause
and every provider stay as they are, and the component name stays available. When several
components fail, the first failure in document order wins, and its `cause` is that component's
error. For a component whose error type is a union (`NotFound | Forbidden`), `cause` is the thrown
member.

## Compilation

`jsxComponent` and `settle` (the async path) in `std/jsx/factory.velt` set `cause` from the caught
error. Both are generic over the component's error type `E`, which is unbounded (a component may
throw any value), and converting a caught `e: E` to `Error | null` is not expressible in Velt:

- `e instanceof Error` on a value of an unbounded type parameter is an error (``instanceof` needs
  a class instance, an interface value or a union with class members, found `E``);
- a class bound `E extends Error` would refuse components that throw strings or other non-`Error`
  values, which `velt/jsx` accepts.

Velt instantiates generic functions per type, so the answer is known in each instantiation. The
smallest piece that uses that is an intrinsic declared in `std/lang/intrinsics/` like the others
(`@intrinsic export declare function asError<T>(value: T): Error | null;`), reachable only from
std:

- **sema**: the declaration is the signature; `crates/velt_sema/src/intrinsic_decls.rs` maps the
  name to a new `Intrinsic` variant.
- **lowering**, per instantiation with the argument's type substituted:
  - a class type that extends `Error` is upcast, and the value is shared (no move);
  - a union matches its members at run time: a member that is a class extending `Error` gives its
    value as above, any other member gives `null`;
  - any other type is `null`.

The end-to-end test renders a sync component throwing a `NotFound`, an async component rejecting
with one, and a component throwing a string: `renderToString` and `renderToStringSync` throw a
`RenderError` whose `cause` narrows to `NotFound` (with its fields) in the first two cases and is
`null` in the third.

## Compatibility

`cause` is TypeScript's `Error.cause` (ES2022), so the narrowing above reads as it does in
TypeScript. React's `renderToString` rethrows the component's error itself; Velt keeps the
wrapper, which carries the component name, and exposes the original through `cause`.

## Alternatives considered

**`JSX.Element<E>`.** Elements generic over the union of their components' errors, so
`renderToString<E>(el: Element<E>): Promise<string> throws RenderError | E`. The static types are
exact, but every component signature, every container of elements (`JSX.Element[]`, props) and
the provider contract would carry `E`, and TypeScript's `JSX.Element` is not generic, so
components shared with a client would no longer compile with `tsc`.

**Rethrow the original, as React does.** Rendering throws the component's error itself, statically
typed as `Error`. `catch (e) { if (e instanceof NotFound) … }` reads like React, but the
component name is lost (React reports it through `onError`), errors that are not `Error`
subclasses still need a wrapper, and one `catch` sees two shapes. A `cause` field keeps one shape
and the name.

**`instanceof` on a type parameter, decided per instantiation.** It would let std write
`e instanceof Error ? e : null` directly. That is a language change and a difference from
TypeScript, which decides `instanceof` at run time; the intrinsic is the smaller step.

## Limitations

Error boundaries (a component that renders a fallback for a failing subtree) are a provider
feature; with `cause` a provider can write them without further compiler support.
