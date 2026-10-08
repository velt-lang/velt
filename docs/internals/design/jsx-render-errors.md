# Design: typed causes of render errors (#82)

Status: proposal. Needs one std-internal intrinsic (below); sema and lowering own it.

## Problem

A component that throws, or an async component whose promise rejects, makes `renderToString`,
`renderToStringSync` and `renderToStream` throw a `RenderError { component; message }`. The
original error survives only as text (`message` embeds `${e}`), so a page cannot react to what
went wrong:

```ts ignore
try {
  html = await renderToString(page(id));
} catch (e) {
  // e: RenderError. Was it the `NotFound` that `Post` threw? Only its text says so.
}
```

`JSX.Element` is not generic over error types, so the type of a component's error can't travel
with the element to the call that renders it.

## Options

**A. `RenderError.cause: Error | null`.** ES2022's `Error.cause`, which TypeScript users know:
the original error when it is a class extending the prelude `Error` (what Velt code throws
almost always), `null` for other thrown values, whose text stays in `message`. The page narrows
the cause with `instanceof`, which already works on an `Error | null` field:

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
and every provider stay as they are; the component name stays available.

**B. `JSX.Element<E>`.** Elements generic over the union of their components' errors, so
`renderToString<E>(el: Element<E>): Promise<string> throws RenderError | E`. Exact static types,
but every component signature, every container of elements (`JSX.Element[]`, props) and the
provider contract would carry `E`, and TypeScript's `JSX.Element` is not generic, so components
shared with a client would no longer compile with `tsc`. Rejected.

**C. Rethrow the original (React's `renderToString`).** Rendering throws the component's error
itself, statically typed as `Error`. `catch (e) { if (e instanceof NotFound) … }` reads like
React, but the component name is lost (React reports it through `onError`), errors that are not
`Error` subclasses still need a wrapper, and one `catch` sees two shapes. Rejected in favour of
A, which keeps one shape and the name.

## Recommendation: A

- `RenderError` gains `cause: Error | null`; `message` and `component` stay as they are.
- `jsxComponent` and the async path (`settle`) set it from the caught error.
- The docs (`docs/std/jsx.md`, the TSX reference) show the `instanceof` narrowing above.

### The missing piece: a caught `E` as an `Error`

`jsxComponent<P, E>(component: (props: P) => Element throws E, …)` catches an `e: E`. Turning
it into `Error | null` is not expressible in Velt today (checked on main):

- `e instanceof Error` on a type parameter is an error (``instanceof` needs a class instance, an
  interface value or a union with class members, found `E``);
- a class bound `E extends Error` is not allowed (``Error` is not an interface``);
- an interface bound (`E extends { message: string }`) doesn't convert `e` to an interface
  value, and `instanceof` on a nullable interface value is an error.

Velt instantiates generic functions per type, so the answer is known in each instantiation. The
smallest piece that uses that is a std-internal intrinsic, like `__intrinsic_array_with_capacity`:

```ts ignore
// `e` as an `Error` when its type is a class extending `Error` (or a union of such classes and
// others: the member's value, else null); null for any other type. Shares `e` (no move).
__intrinsic_as_error(e): Error | null
```

- **sema** (`body/expr/builtins.rs` `intrinsic_call`): one argument of any type, result
  `Error | null`.
- **lowering**, per instantiation with the argument's type substituted:
  - a class type that extends `Error` upcasts and shares the value;
  - a union matches its members (classes extending `Error` as above, the rest `null`);
  - any other type is `null`.

Alternatives to the intrinsic, for whoever owns the type checker: allowing `instanceof` on a
value of type-parameter type, decided per instantiation, would let std write
`e instanceof Error ? e : null` directly. That is a language change (and a TypeScript
difference: TS decides `instanceof` at run time), so the intrinsic is the smaller step.

## Tests (when implemented)

- A golden with a sync component throwing a `NotFound`, an async component rejecting with one,
  and a component throwing a string. `renderToString`, `renderToStringSync` and
  `renderToStream` throw a `RenderError` whose `cause` narrows to `NotFound` (with its fields)
  in the first two cases and is `null` in the third.
- The first failure in document order still wins, and its `cause` is that component's error.
- A component whose error type is a union (`NotFound | Forbidden`) gives the thrown member.

## Out of scope

Error boundaries (a component that renders a fallback for a failing subtree) are a provider
feature; with `cause` they can be written in a provider without further compiler support.
