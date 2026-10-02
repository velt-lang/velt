# Error handling

Velt keeps JavaScript's `throw`, `try`, `catch` and `finally`, and adds what TypeScript can't:
the compiler knows exactly which errors every call can throw. There is no `unknown` in a
`catch`, no unwinding, and no `Result` type to thread through your code.

## Throwing and catching

Errors are usually classes extending the built-in `Error`, which has a `message`:

```ts
class NotFound extends Error {
  id: string;

  constructor(id: string) {
    super(`no user ${id}`);
    this.id = id;
  }
}

class Forbidden extends Error {}

function loadUser(id: string): string {
  if (id == "0") throw new Forbidden("admins only");
  if (id != "42") throw new NotFound(id);
  return "ada";
}

function describe(id: string): string {
  try {
    return `user ${loadUser(id)}`;
  } catch (e) {                       // e: NotFound | Forbidden
    if (e instanceof NotFound) {
      return `missing ${e.id}`;       // e is a NotFound here
    } else {
      return `denied: ${e.message}`;  // and a Forbidden here
    }
  }
}

console.log(describe("42"), describe("7"), describe("0"));
// user ada missing 7 denied: admins only
```

- You don't declare what `loadUser` throws: the compiler infers `NotFound | Forbidden` and
  gives that type to `e`.
- Narrow `e` with `instanceof` (or `switch`, `typeof`, `==`), like any union. Once every member
  is handled, the compiler knows the `if`/`else` chain covers all cases.
- A call that may throw, outside a `try`, makes its caller throw the same errors. Errors
  propagate through callbacks too: `ids.map((id) => loadUser(id))` throws what `loadUser`
  throws.
- `finally` runs on every path, as in JavaScript. For cleanup tied to a value, prefer
  [`using`](memory.md#cleanup-runs-at-a-known-point).

## `throws` clauses

Write a `throws` clause when you want the compiler to hold a function to a contract, typically
on module boundaries:

```ts
class Timeout extends Error {}
class Refused extends Error {}

function connect(host: string): i64 throws Timeout | Refused {
  if (host == "") throw new Refused("empty host");
  return 3;
}
```

The body may throw only what the clause allows (``` `connect` throws `Overflow`, which its
`throws` clause does not allow ```), and callers see exactly the declared type. Function types
carry error types too: `(x: string) => i64 throws ParseError`. A function type without
`throws` accepts only functions that can't throw.

## Errors as values

Sometimes a failure is an expected outcome, not an exception. Return a union:

```ts
class NotFound {
  key: string;
  constructor(key: string) {
    this.key = key;
  }
}

function lookup(m: Map<string, i64>, key: string): i64 | NotFound {
  const v = m.get(key);
  return v == null ? new NotFound(key) : v;
}

const prices = new Map<string, i64>();
prices.set("tea", 3);
const r = lookup(prices, "milk");
if (r instanceof NotFound) {
  console.log("no price for", r.key);
} else {
  console.log(r + 1);
}
```

Or turn any throwing call into a value with `attempt`:

```ts
class ParseError extends Error {}

function parsePort(s: string): i64 throws ParseError {
  const n = Number(s);
  if (n != n || n < 1.0 || n > 65535.0) throw new ParseError(`bad port: ${s}`);
  return n as i64;
}

const port = attempt(() => parsePort("80x"));   // i64 | ParseError
console.log(port instanceof ParseError ? port.message : `port ${port}`);   // bad port: 80x
```

## Errors in async code

A promise's type carries what it can reject with (`Promise<T, E>`), and `await` rethrows it, so
`try`/`catch` around `await` is typed the same way. An async function may have a `throws`
clause: `async function load(): Promise<User> throws NotFound`. A spawned task whose result
nobody awaits reports its error as uncaught. See [Async and concurrency](async.md#errors-in-concurrent-work).

## Panics are bugs

Some failures are programming errors, not conditions to handle: an index out of bounds,
integer division by zero, a failed `assert`, `unwrap()` on `null`, an explicit `panic(msg)`.
These **panic**: the program prints `panic: <message> at file:line:col` and exits with code
101. Panics can't be caught.

An error that escapes `main` prints `Uncaught <Class>: <message> at file:line:col` and exits with
code 1. It names the error's actual class, even when the function declared a base class such as
`throws Error`, and the line it was thrown on; an error from the standard library (a missing
file, say) points at the line of your code that made the call.

## Designing errors for an application

- One base class per area (`ApiError`, `ConfigError`) with the data callers need (`status`,
  `code`), and subclasses for specific cases. A `catch` that only needs the base fields can read
  them without narrowing when everything thrown extends the base.
- Convert errors at boundaries: wrap an `IoError` or a `JsonError` into your own type where you
  know what it means (see the [CLI guide](cli-app.md) and the [HTTP guide](http-server.md)).
- Use unions for expected outcomes (`User | NotFound`) and exceptions for failures that should
  travel up the stack.

## How it compiles

Each throwing function returns a tagged result; each call checks the tag and branches. There
are no exception tables and no stack unwinding, so `try` costs nothing when nothing is thrown,
and a throw costs one return per frame.
