# Variables and conditions

## `const` and `let`

- `const` bindings cannot be reassigned (``cannot assign twice to const `x` ``); `let`
  bindings can. Both are block-scoped. `let x: T;` may be assigned later.
- `const` only fixes the binding: modifying a `const` object or array is fine, as in JS.
- `using` and `await using` declare a `const` that is disposed at the end of the block
  ([Memory model](memory.md#resource-cleanup-using-and-symboldispose)).
- There is no `var`.

## Conditions: truthiness

Conditions (`if`, `while`, `do … while`, `for`, `?:`) and the operands of `!`, `&&` and `||`
test values of any type as JavaScript does.

- Falsy are `false`, `null`, `0` and `-0` (of `number` and of every integer type: `0` of `i64`,
  `u8`, …), `NaN` and `""`. Everything else is truthy: `" "`, `"0"` and `"false"`, `Infinity`,
  and every object, array and function (also an empty array).
- A nullable is truthy when it is not `null` and its payload is truthy, and the test narrows it
  like `x !== null` does: `if (!user) return;` leaves `user` non-null below, and
  `if (user && user.manager)` narrows both. `if (name)` on a `string | null` makes `name` a
  `string` (which may still be `""` in the `else` branch). `??` still replaces only `null`: for
  `n: number | null` holding `0`, `n ?? 5` is `0` and `n || 5` is `5`.
- An enum value is falsy when its member's value is `0` or `""`. A union is tested by the
  member it holds.
- `void` values are not conditions (``an expression of type `void` cannot be tested for
  truthiness``), and neither are values of a generic type, whose test would depend on the type
  argument.
- A test is one comparison in the compiled code: `n != 0` on an integer, `x != 0 && x == x` on
  a `number` (one compare once optimized), `s.length != 0` on a string.
- `||` and `&&` return an operand, as in JavaScript: `a || b` is `a` when `a` is truthy and
  `b` otherwise; `a && b` is `b` when `a` is truthy and `a` otherwise. `b` runs only when it is
  the result. The type is TypeScript's: when both sides have the same type, that type
  (`count || 8080` is a `number`, `name || "anon"` a `string`), else their union
  (`count || "none"` is a `number | string`). `||` drops `null` from the left side's type
  (`(n: number | null) || 0` is a `number`). On a nullable object, `a || b` is `a ?? b` and
  `a && b` is `b` or `null` (type `B | null`: `user && user.name` is a `string | null`). When a
  `boolean` is expected (conditions, `const ok: boolean = …`) or both sides are `boolean`s,
  they give a `boolean`.
- `x ||= v` and `x &&= v` assign when `||` or `&&` would take the right side.

```ts
function label(count: number, name: string | null): string {
  if (!count) {
    return "none";
  }
  if (name) {
    return `${count} for ${name}`;    // name is a string here
  }
  return `${count}`;
}

function main() {
  const port = 0;
  const retries: i64 = 3;
  console.log(port || 8080, retries && retries - 1);   // 8080 2
  console.log(label(0, "ann"), label(2, ""), label(2, "ann"));   // none 2 2 for ann
  const pairs = [[1, 2], [0, 5], [1, 1]];
  pairs.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
  console.log(JSON.stringify(pairs));  // [[0,5],[1,1],[1,2]]
}
```

## No mutable module state

Module scope holds only constants, functions and types. A module-level `let` is an error
("mutable module-level state is not allowed"; in a root file with top-level statements, a `let`
that only those statements use is a local of the generated `main`, see
[Scripts](modules.md#scripts-top-level-statements)), and module `const` initializers must be literals
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
  if (requestCount) {
    console.log("never");
  }
}
```
