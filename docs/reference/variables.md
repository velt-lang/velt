# Variables and conditions

## `const` and `let`

- `const` bindings cannot be reassigned (``cannot assign twice to const `x` ``); `let`
  bindings can. Both are block-scoped. `let x: T;` may be assigned later.
- `let x;` without a type takes its type from its first assignment, as in TypeScript (also one
  inside a `try` or an `if`); later assignments must have that type. Reading it, updating it
  (`x += 1`) or using it in a closure before that assignment is an error asking for a type
  (TypeScript reads `undefined` there), as is a `let x;` that is never assigned.

  ```ts
  function parse(s: string): number {
    let n;
    try {
      n = Number.parseInt(s, 10);
    } catch (e) {
      return -1;
    }
    return n;
  }
  console.log(parse("42")); // 42
  ```
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
- A test is one comparison in the compiled code: `n != 0` on an integer (also on a `number` the
  compiler stores as an integer), `x != 0 && x == x` on any other `number` (one compare once
  optimized), `s.length != 0` on a string.
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
[Scripts](modules.md#scripts-top-level-statements)), and module `const` initializers must be
constant expressions (literals, struct literals of constants, other module constants, closures)
or [calls of pure functions](#module-constants-initialized-by-a-call). State that changes lives in values created by `main` (for
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

## Module constants initialized by a call

A module constant is not stored: its initializer is evaluated where the constant is used. A
call can initialize one when that call is *pure*, so that evaluating it at each use gives what
TypeScript's single evaluation at load gives:

- the callee is a named function (a free function, a static method or an imported function,
  generic or not), not a function value or a method of a value;
- each argument is a constant expression, another module constant, a function or a closure;
- the callee and everything it calls have no loops, no recursion, no `throw`, no `await`, no
  calls through function values, and no effects (I/O, time, randomness, `console`, `shared`,
  `spawn`, external functions). It may allocate, construct objects and create closures, as
  `component` does below: the closures it returns run later as ordinary code;
- a closure it creates keeps no mutable state: it captures no variable that is assigned, and no
  object, array or class instance (``it returns a closure that keeps mutable state (`n`)``).
  In Node the constant is one closure whose state carries over from call to call; a new
  closure at each use would start over each time;
- the result is a value: a number, string, struct, tuple, enum or function. Class instances,
  arrays and maps are rejected (a new array at each use would change what `X.push(1)` does).
  An object or struct constant can't be moved into a variable or a field (``cannot move out of
  module constant `UNIT` ``; `UNIT.clone()` makes an owned copy): TypeScript would alias it,
  so a change through the copy would show in the constant.

```ts
type Props = { start: number };
type Component = (props: Props) => string;

function component(setup: (props: Props) => () => string): Component {
  return (props: Props): string => setup(props)();
}

const Counter = component((props: Props) => {
  const count = props.start + 1;
  return (): string => `Count: ${count}`;
});

type Point = { x: number; y: number };

function point(x: number, y: number): Point {
  return { x: x, y: y };
}

const ORIGIN = point(0, 0);

function main() {
  console.log(Counter({ start: 1 }), ORIGIN.x);   // Count: 2 0
}
```

A call that does not qualify is an error that says why and where, through the call chain, with
the fix: compute the value in `main` and pass it on, or make the constant a function.

```ts error
function load(): string {
  console.log("loading");
  return "data";
}

const DATA = load();                    // error: `load` cannot initialize module constant `DATA`:
                                        // it calls `console.log`, which has effects
function main() {
  console.log(DATA);
}
```

Evaluated at each use, such a constant is a new object or closure each time, so comparing it by
identity would always be false where TypeScript says `true`. `===` and `!==` with a module
constant compared by identity (objects, arrays, functions) are an error: ``Counter` is evaluated
at each use, so comparing it by identity is always false``.

```ts error
function adder(n: number): (x: number) => number {
  return (x: number): number => x + n;
}

const ADD_ONE = adder(1);

function main() {
  console.log(ADD_ONE === ADD_ONE);     // error: `ADD_ONE` is evaluated at each use
}
```

**Known difference.** A function-valued constant passed on is a new closure at each use: after
`const a = ADD_ONE; const b = ADD_ONE;`, `a === b` is `false` (TypeScript: `true`), and so is
any identity comparison the constant reaches by being passed, which the compiler cannot see:
`[ADD_ONE].indexOf(ADD_ONE)` is `-1`, and an event emitter's `off(ADD_ONE)` does not find the
handler that `on(ADD_ONE)` added. **Planned**: evaluating such a constant once, as Node does,
which removes this difference ([#802](https://github.com/velt-lang/velt/issues/802)). Until then,
copy the constant into a local once and pass the local:

```ts
type Handler = (x: number) => number;

function adder(n: number): Handler {
  return (x: number): number => x + n;
}

const ADD_ONE = adder(1);

function main() {
  const handlers: Handler[] = [];
  const h = ADD_ONE;                    // one value, passed on
  handlers.push(h);
  console.log(handlers.indexOf(h));     // 0 (`handlers.indexOf(ADD_ONE)` would be -1)
}
```

A panic (an integer overflow,
an index out of bounds) in an initializer happens at the use, where TypeScript would throw at
module load.
