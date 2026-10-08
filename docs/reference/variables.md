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
  if (requestCount) {                   // error: numbers are not conditions (`requestCount !== 0`)
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
- the result is a value: a number, string, struct, tuple, enum or function. Class instances,
  arrays and maps are rejected (a new array at each use would change what `X.push(1)` does).

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

**Known difference.** A copy of such a constant in a local is one value, and compares equal to
itself, but two copies are two values: after `const a = ADD_ONE; const b = ADD_ONE;`, `a === b`
is `false` (TypeScript: `true`). The compiler cannot see this case. A panic (an integer overflow,
an index out of bounds) in an initializer happens at the use, where TypeScript would throw at
module load.
