# Call variants

Status: accepted (implemented). Issue: [#3515](https://github.com/velt-lang/velt/issues/3515).

## Summary

`@variant("plain", ...when)` on a function says that a call whose arguments show, at compile
time, that it needs less of the function calls `plain` instead: another function of the same
module that takes the same arguments. Each condition in `when` names an argument and what shows
it: a string literal that starts with some text (`{ arg: 0, startsWith: "http:" }`), an object
literal without some property (`{ arg: 0, lacks: "tls" }`), or an object literal whose property is
one of some string literals (`{ arg: 1, field: "redirect", oneOf: ["manual", "error"] }`). A call
takes `plain` when every condition holds; every other call is the function itself.

A program reaches what only the function's full form calls through the calls that may need it.
Since the checker loads and checks a std module only once live code reaches it
([lazy std](lazy-std.md)), a costly part that some arguments never need is neither loaded,
checked nor built for a program whose calls all show they don't: `fetch("http://…", { redirect:
"manual" })` has no TLS client, `connectWebSocket("ws://…")` neither, and `serve({ port: 8080 }, …)`
no TLS server. A plain `fetch("http://…")` keeps the client: it follows redirects, and one may lead
to `https:`, as in Node.

## Motivation

`fetch` takes `http:` and `https:` URLs, and decides which at run time from the URL's text. Its
`https:` path holds the TLS client, the certificate checks and the crypto under them: about
50,000 lines of std, more than everything else a small `fetch` program loads. A program that only
ever writes

```ts
const res = await fetch(`http://127.0.0.1:${port}/health`, { redirect: "manual" });
```

can never take that path, but the checker sees one `fetch` function, and the call of the TLS
client in it is live. The same holds for `http-server`'s `serve`, which binds with `listenTls`
when its options carry `tls`:

```ts
import { serve } from "http-server";

const server = await serve({ port: 8080 }, (req: Request) => new Response("hi"));
```

Splitting each such entry point by hand (`fetchHttp`, `serveTls`) would put a performance detail
into the API. The literal at the call already says which form it needs.

## Design

The decorator is exported by `velt/compiler`:

```ts
import { variant } from "velt/compiler";

@variant(
  "fetchPlain",
  { arg: 0, startsWith: "http:" },
  { arg: 1, field: "redirect", oneOf: ["manual", "error"] },
)
export async function fetch(input: string | URL | Request, init: RequestInit = {}): Promise<Response> {
  return await fetchWith(input, init, tlsOn);
}

async function fetchPlain(input: string | URL | Request, init: RequestInit = {}): Promise<Response> {
  return await fetchWith(input, init, null);
}
```

- `plain` names a top-level function of the decorated function's module, exported or not, with
  as many parameters and type parameters. It must give the same result as the decorated function
  for every call whose arguments pass `when`: the decorator changes which code a program builds,
  never what a call does.
- `when` is one condition or more; a call takes `plain` when all hold. Each names the index of
  the parameter whose argument it tests (`arg`, 0 for the first) and one test:

| Test | Argument `arg` passes when it is |
|---|---|
| `startsWith: s` | a string literal starting with `s`, or a template literal whose text before its first `${…}` starts with `s` (ASCII letters in either case, as URL schemes compare) |
| `lacks: p` | an object literal with no property `p` and no spread, or left out |
| `field: p, oneOf: [v…]` | an object literal without a spread whose property `p` is a string literal among the `v` |

- The decorated function is a top-level function, not a generator. Its value (`const f = fetch`)
  is the function itself, and so is every call whose argument does not pass: a variable, a
  constant, a property, a call.
- A call that takes `plain` is checked as a call of `plain`: its explicit type arguments, the
  argument checks and what it throws are `plain`'s.

## Compilation

- `crates/velt_sema/src/decorators/known.rs` (`variant`) finds the decorator by its role
  (`velt_common::std_hooks` `decorator_variant`), checks it and records `FnInfo::variant`.
- `crates/velt_sema/src/body/expr/call.rs` (`fn_call`) tests the argument's syntax and calls
  `plain` when it passes. Nothing else changes: the call is an ordinary `hir::ExprKind::Call` of
  `plain`, so liveness, lowering and the backends see a call of `plain`.

## Runtime cost

None: a call is a call of one of two functions. At compile time it can only remove work: the
`http:` `fetch` program that follows no redirect, the `ws:` WebSocket client and the plain `serve`
program above load, check and build none of the TLS code.

## Diagnostics

On the decorated function, at the decorator:

- `` `@variant` applies to a top-level function that is not a generator ``
- `` `@variant` names `fetchPlan`, which is no function of this module ``
- `` `@variant`'s `fetchPlain` must be a top-level function with the same parameters and type parameters ``
- `` `@variant` takes the plain function's name and its conditions ``
- `` `@variant` needs a condition ``
- `` a `@variant` condition is an object literal ``
- `` a `@variant` condition names its argument (`arg`) ``
- `` `@variant`'s argument index 2 is not one of the function's 2 parameters ``
- `` a `@variant` condition has one test: `startsWith`, `lacks`, or `field` with `oneOf` ``
- `` `@variant`'s `oneOf` lists strings ``

## Compatibility

TypeScript has no decorators on functions; `velt/compiler`'s decorators are Velt's own, and a
program that uses none runs as in Node. Each variant in std gives what Node gives: `fetch` takes
`fetchPlain` only when it cannot follow a redirect (`redirect: "manual"` or `"error"`), so an
`http:` URL redirected to `https:` is fetched as in Node.

## Alternatives considered

**Overloads on literal types** (`function fetch(input: \`http:${string}\`)`). TypeScript's
overloads share one implementation, so the implementation, and the TLS client it calls, stay live;
and template literal types with a `string` hole are not supported outside conditional types.

**A word scan of the program's source.** Loading the TLS code when the program's text names
`listenTls` or `connectTls` misses options that arrive as JSON, counts words in template literals,
and loads the code for a package that names it but is never asked to use it.

**Constant propagation into `fetch`.** Folding `t.https` to `false` from the call's literal needs
the URL parser at compile time, and comes too late: loading is decided by the checker, before
lowering.

## Limitations

- Only the call's own syntax decides: `const URL = "http://…"; fetch(URL, { redirect: "manual" })`
  takes the function itself.
- `plain`'s equivalence with the decorated function is the author's promise; the compiler checks
  only the shape of its signature.
