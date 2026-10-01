# Variables and conditions

## `const` and `let`

- `const` bindings cannot be reassigned (``cannot assign twice to const `x` ``); `let`
  bindings can. Both are block-scoped. `let x: T;` may be assigned later.
- `const` only fixes the binding: modifying a `const` object or array is fine, as in JS.
- `using` and `await using` declare a `const` that is disposed at the end of the block
  ([Memory model](memory.md#resource-cleanup-using-and-symboldispose)).
- There is no `var`.

## Conditions: safe truthiness

Conditions (`if`, `while`, `do … while`, `for`, `?:`) and the operands of `!`, `&&` and `||`
accept `bool` and nullable values.

- A nullable is truthy when it is not `null`, and the test narrows it like `x !== null` does:
  `if (!user) return;` leaves `user` non-null below, and `if (user && user.manager)` narrows
  both. `bool | null` is truthy only when it is `true`.
- Numbers, strings and enums are rejected in conditions and as operands of `!`, `&&` and `||`,
  also as the payload of a nullable: `n: i64 | null` in `if (n)` would mix the null test with
  JavaScript's falsy `0`, so it is an error. Write `count !== 0`, `name !== ""` or
  `n !== null` (each error says which; editors offer it as a quick fix), and `??` for defaults
  (`port || 8080` is an error with "use `??` for a default").
- On nullable objects, `||` and `&&` return values like TypeScript: `a || b` is `a ?? b` (`T`
  when `b` is a `T`), and `a && b` is `b` when `a` is not null (with `a` narrowed in `b`), else
  `null` (type `B | null`: `user && user.name` is a `string | null`). When a `bool` is expected
  (conditions, `const ok: bool = …`) or the left side is a `bool`, they are logical and give a
  `bool`.

## No mutable module state

Module scope holds only constants, functions and types. A module-level `let` is an error
("mutable module-level state is not allowed"), and module `const` initializers must be literals
or struct literals of constants. State that changes lives in values created by `main` (for
example `shared(...)` or class instances) and is passed where it is needed. This keeps request
handlers free of data races, and it is what lets `velt dev` swap code in a running program
without migrating globals ([hot reload](../tooling/dev.md)).

```ts
const MAX_USERS: usize = 100;           // module constant: fine

class User {
  name: string = "ann";
  manager?: User;
}

function managerName(user: User | null): string {
  if (!user) {
    return "nobody";                    // a null check: user is a User below
  }
  return (user.manager && user.manager.name) ?? "none";   // `&&` gives string | null
}

function main() {
  const hits = shared(0);               // state lives in values main creates
  const names: string[] = [];
  names.push("ann");                    // modifying a const array is fine
  let count = names.length;
  if (count !== 0 && count < MAX_USERS) {
    hits.add(1);
  }
  console.log(hits.get(), count, managerName(new User()), managerName(null));
}
```

```ts error
let requestCount = 0;                   // error: mutable module-level state is not allowed

function main() {
  if (requestCount) {                   // error: numbers are not conditions (`requestCount !== 0`)
    console.log("never");
  }
}
```
