# Errors

`throw`, `try`, `catch` and `finally` have JavaScript syntax and TypeScript-style types, and they
compile to zero-cost result returns: every throwing call is "call, check a tag, branch". There
is no unwinding and there are no exception tables. Errors are **typed and checked**: the
compiler knows exactly what each function and each `try` block can throw.

## Throwing

- `throw expr` throws a value of any type, usually a class extending the prelude `Error`, which
  has a `message` field. Calling a throwing function outside `try` makes the caller throw it
  too.
- A function's **error type** is the union of everything it can throw. It is inferred over the
  whole program (also through recursion), or written as a **`throws` clause** after the return
  type: `function load(id: string): User throws NotFound | Forbidden`.
- A written clause bounds the body
  (``` `load` throws `Timeout`, which its `throws` clause does not allow ```; a subclass is
  allowed by its base class) and is the function's error type even when the body throws less.
- Methods, constructors (`constructor(x: T) throws E`), arrows (`(x: T): R throws E => …`) and
  async functions (`async function f(): Promise<T> throws E`, see [Async](async.md#errors))
  take a `throws` clause; `declare function` cannot.

## Catching

- In `catch (e)`, `e` has the **exact union** of what the `try` block can throw
  (`NotFound | Forbidden`; `never` when nothing can throw). Narrow it with `instanceof`,
  `typeof`, `==` or `switch` like any [union](types.md#union-types).
- An `if`/`else if` chain whose branches leave handles every member, so no code is needed after
  it.
- `throw e;` rethrows; a narrowed `e` rethrows just that member.
- `catch { … }` without a binding and `finally` (which runs on every path) work as in JS.
- `instanceof` cannot tell apart subclasses of a member (a downcast); make the subclasses
  members of the union instead.

## Dynamic calls

Function types carry an error type: `(x: T) => U throws E`. Without `throws`, the function
cannot throw.

- A closure gets the error type of the function type expected where it is written (and must
  not throw more); otherwise, what its body throws. A named function used as a value may throw
  less than its function type allows.
- Interface methods and overridden methods share one error type per method: the interface's (or
  base method's) `throws` clause bounds every implementation
  (``` `Db.get` throws `Forbidden`, but `Store.get` does not allow it ```); without one it is the
  union of what the implementations throw.
- **Higher-order functions** propagate their callback's errors by being generic over them:
  `function run<E>(f: () => i64 throws E): i64 throws E`. The prelude's array methods
  (`forEach`, `map`, `filter`, `reduce`, `find`, `findIndex`, `some`, `every`), `Map` methods
  (`forEach`, `upsert`, `update`, `getOrInsert`) and `(T | null).map` do this, so
  `xs.map((x) => parse(x))` throws what `parse` throws, and nothing when the callback cannot
  throw. Sort comparators and `Mutex.with` callbacks cannot throw.

## Errors as values

Return a union (`function find(id: string): User | NotFound`) and narrow it, or turn a throwing
call into a value with `attempt(() => f())`: its type is `T | E` (`E | null` when `f` returns
nothing; `null` means success).

## Uncaught errors and panics

- An error escaping `main` prints `Uncaught <Class>: <message> at file:line:col` to stderr and
  exits with code 1. `<Class>` is the error's actual class (an `IoError` thrown through a
  function declared `throws Error` prints `IoError`), and the location is where it was thrown;
  for an error thrown inside the standard library, it is the line of your code that called
  into it.
- **Panics** are bugs, not errors: an index out of bounds, integer division by zero,
  `panic(msg)`, a failed `assert`. They print `panic: … at file:line:col` and exit with code
  101. They cannot be caught.
- There is no `Result` type, no `Ok`/`Err` and no postfix `?`: errors propagate by themselves,
  and values use unions.

## Inference limits

A closure created inside a recursive function that it calls, and a `catch` or promise whose
error type depends on a function still being checked through recursion, may need a `throws`
clause (``` the error type of this function is not known yet ```). Error types of interface and
overridden methods cannot depend on type parameters.

## Example

```ts
class NotFound extends Error {
  id: string;

  constructor(id: string) {
    super(`no user ${id}`);
    this.id = id;
  }
}

class Forbidden extends Error {}

function load(id: string): string throws NotFound | Forbidden {
  if (id == "0") {
    throw new Forbidden("admins only");
  }
  if (id != "42") {
    throw new NotFound(id);
  }
  return "ada";
}

function describe(id: string): string {
  try {
    return `user ${load(id)}`;
  } catch (e) {                         // e: NotFound | Forbidden
    if (e instanceof NotFound) {
      return `missing ${e.id}`;
    } else if (e instanceof Forbidden) {
      return `denied: ${e.message}`;
    }                                   // every member handled: no return needed here
  }
}

function lengths(ids: string[]): usize[] {
  return ids.map((id) => load(id).length);  // throws what `load` throws
}

try {
  console.log(describe("42"), describe("7"), describe("0"));
  console.log(lengths(["42", "1"]));
} catch (e) {
  console.log("lengths failed:", e.message);
} finally {
  console.log("done");
}
const r = attempt(() => load("7"));         // string | NotFound | Forbidden
console.log(r instanceof NotFound ? `absent ${r.id}` : "found or denied");
```

Async errors (rejections, `Promise.all`, spawned tasks) are covered in
[Async and concurrency](async.md#errors).
