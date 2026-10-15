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
- One declaration may declare several variables, as in TypeScript: `let i: number, j = 0;` is
  `let i: number; let j = 0;` (at the top level, in a block and in a `for` initializer).
- `let x!: T` (TypeScript's definite assignment assertion) declares `x` as `let x: T;` does.
  Velt still checks that `x` is assigned before it is read; a variable assigned only inside a
  closure (where TypeScript needs `!`) is not supported yet.

  ```ts
  let lo: number, hi: number = 10;
  let mid!: number;
  lo = 2;
  mid = (lo + hi) / 2;
  console.log(lo, hi, mid); // 2 10 6
  ```
- `const` only fixes the binding: modifying a `const` object or array is fine, as in JS.
- `using` and `await using` declare a `const` that is disposed at the end of the block
  ([Memory model](memory.md#resource-cleanup-using-and-symboldispose)).
- There is no `var`.

## Conditions: truthiness

Conditions (`if`, `while`, `do … while`, `for`, `?:`) and the operands of `!`, `&&` and `||`
test values of any type as JavaScript does.

- Falsy are `false`, `null`, `undefined`, `0` and `-0` (of `number` and of every integer type: `0` of `i64`,
  `u8`, …), `NaN` and `""`. Everything else is truthy: `" "`, `"0"` and `"false"`, `Infinity`,
  and every object, array and function (also an empty array).
- A nullable (`T | null` or `T | undefined`) is truthy when it is neither `null` nor `undefined`
  and its payload is truthy, and the test narrows it like `x != null` does: `if (!user) return;` leaves `user` non-null below, and
  `if (user && user.manager)` narrows both. `if (name)` on a `string | null` makes `name` a
  `string` (which may still be `""` in the `else` branch). `??` still replaces only `null` and `undefined`: for
  `n: number | null` holding `0`, `n ?? 5` is `0` and `n || 5` is `5`.
- An enum value is falsy when its member's value is `0` or `""`. A union is tested by the
  member it holds.
- A value of a generic type `T` is tested as a value of the type `T` stands for: generic code
  is compiled for each type argument, so `!x` in `function f<T>(x: T)` is the number test in
  `f(0)` and the string test in `f("")`, with no check at run time of what `T` is.
- `void` values are not conditions (``an expression of type `void` cannot be tested for
  truthiness``).
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
- `c ? a : b` is typed the same way: when one branch converts to the other's type, that type,
  else their union (`qty ? qty : "none"` is a `number | string`).

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
  console.log(port ? port : "none");   // none
  console.log(label(0, "ann"), label(2, ""), label(2, "ann"));   // none 2 2 for ann
  const pairs = [[1, 2], [0, 5], [1, 1]];
  pairs.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
  console.log(JSON.stringify(pairs));  // [[0,5],[1,1],[1,2]]
}
```

## Module state

A module-level `let` is module state, as in Node: functions, methods and closures read and
assign it, and every module that imports it sees its current value (an ES live binding). Its
initializer runs once, when the program starts, in Node's module evaluation order: a module
after the modules it imports (depth first, in the order of the `import` statements), and the
declarations of one module in source order.

```ts
let count = 0;
let current: string | null = null;
const seen = new Map<string, number>();   // not a constant value: kept like a `let`

function enter(name: string) {
  current = name;
  count++;
  seen.set(name, (seen.get(name) ?? 0) + 1);
}

function main() {
  enter("a");
  enter("b");
  enter("a");
  const add = (n: number) => {
    count += n;                         // closures read and write the module's `count`
  };
  add(10);
  console.log(count, current, seen.get("a"));   // 13 a 2
}
```

- **Per task.** Each task has its own module state: a task started with `spawn` (and an HTTP
  request handler, which runs as a task) starts from the initializers, like a Node `Worker`
  that loads the modules again. Within a task it behaves exactly like Node's single thread,
  across `await`s and started promises. State that tasks share is `shared(...)` passed to
  them ([Thread safety](async.md#thread-safety)). On WebAssembly, which runs every task on one
  thread, the tasks share one module state.
- **Objects are shared, not copied**: `const a = items` refers to the module's array, so
  `a.push(1)` shows in `items`, as in JS.
- **Imports are read-only bindings**: assigning an imported `let` is an error
  (``cannot assign to imported binding `x` ``), as in JavaScript; export a function that
  assigns it. Modifying its contents (`items.push(x)`) is fine.
- A `const` whose value is not a constant (`new Map()`, an array of objects, a call with
  effects) is kept the same way, evaluated once per task; assigning it is an error.
- An initializer that reads a `let` declared after it (whose initializer has not run yet) is
  an error (``` `b` is read before its initializer runs ```), where JavaScript throws a
  `ReferenceError`. A read through a function the initializer calls is not caught.
- In a root file with top-level statements, a `let` that only those statements use is a local
  of the generated `main` ([Scripts](modules.md#scripts-top-level-statements)); one that a
  function uses is module state, and its initializer runs before the first statement.
- Cost: a program without module state pays nothing. Each read or write of a module-level
  `let` is a call into the runtime that returns the task's state (a thread-local read).
- `velt dev`: code swapped into a running program starts its module state over, as after a
  restart ([hot reload](../tooling/dev.md)).

```ts error
let a = b + 1;                          // error: `b` is read before its initializer runs
let b = 1;

function main() {
  console.log(a, b);
}
```

## Tables

A module constant can be an array of numbers or booleans whose elements are literals: a table.
Its elements are stored once in the program as read-only data, and `K[i]`, `K.length` and
`for (const x of K)` read them there: no array is built, and an element costs what an element of
a local array costs.

```ts
const POWERS: u64[] = [1, 10, 100, 1000];
const HEX: u8[] = [48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 97, 98, 99, 100, 101, 102];
const SIGNS: number[] = [-1, 0, 1];

function digits(n: u64): usize {
  let count: usize = 1;
  for (const p of POWERS) {
    if (n >= p * 10) {
      count += 1;
    }
  }
  return count;
}

function main() {
  console.log(POWERS.length, POWERS[2], digits(4321), HEX[10], SIGNS.includes(0));
  const mine = POWERS.slice();          // a copy is an ordinary array
  mine.push(10000);
  console.log(mine.length);
}
```

- Each element is a number or boolean literal, also negated or converted with `as`, or a module
  constant that is one. Any other array constant (strings, nested arrays, objects, computed
  elements) is not a table but [module state](#module-state), evaluated once per task.
- Used in any other way, a table is what every module constant is: evaluated at that use, here
  to a new array with those elements. `K.includes(x)`, `f(K)` and `K.slice()` each build one, so
  in a hot loop index the table or iterate it instead of passing it along.
- Nothing can modify a table or take it over, and a use that could is a compile error. That is
  `K[0] = 1`, `K.push(1)`, `K.fill(0)`, and passing `K` to a function that modifies its
  parameter (``cannot modify module-level constant `K` ``), as for any module constant. For a
  table it is also:
  - passing it to a function value (`const f = (xs: u8[]) => …; f(K)`, `fs[0](K)`, a callback
    parameter), or to a function together with a function value that could modify it
    (``cannot pass module constant `K` to a function value``): the compiler can't see what that
    function does;
  - binding it to a name or a field, however it is named: `const mine = K`, `K!`, `ns.K` for an
    imported table, `Cls.K` for a `static readonly` one (``cannot move out of module constant
    `K` ``); an `async` function, which takes over its arguments, is the same.

  `K.slice()` is the copy to use there: an ordinary array to keep, modify or pass anywhere.
- Comparing a table by identity (`K === K`) is an error, like for other module constants.

## Module constants initialized by a call

A module constant whose initializer is a call is evaluated where it is used when that call is
*pure*, so that evaluating it at each use gives what TypeScript's single evaluation at load
gives; any other initializer (a call with effects, a class instance, a `Map`) makes it
[module state](#module-state), evaluated once per task:

- the callee is a named function (a free function, a static method or an imported function,
  generic or not), not a function value or a method of a value;
- each argument is a constant expression, another module constant, a function or a closure;
- the callee and everything it calls have no loops, no recursion, no `throw`, no `await`, no
  calls through function values, and no effects (I/O, time, randomness, `console`, `shared`,
  `spawn`, external functions). It may allocate, construct objects and create closures, as
  `component` does below: the closures it returns run later as ordinary code;
- a closure it creates keeps no mutable state: it captures no variable that is assigned, and no
  object, array or class instance. Otherwise the constant is module state, one closure whose
  state carries over from call to call, as in Node;
- the result is a value: a number, string, struct, tuple or enum. A function from a call is
  kept as module state, so that it is one value as in Node, and so are class instances, arrays
  and maps (a new array at each use would change what `X.push(1)` does; an array of number
  literals is a [table](#tables), which needs no call).
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

A call that does not qualify makes the constant [module state](#module-state): it is
evaluated once per task, before `main`, in module order, as Node evaluates it once at load.
A call that may throw or is `async` stays an error (``it may throw``): nothing could catch or
await it there.
A function from a call is kept the same way, so it is one value wherever it goes:

```ts
type Handler = (x: number) => number;

function adder(n: number): Handler {
  return (x: number): number => x + n;
}

function load(): string {
  console.log("loading");               // runs once, before main
  return "data";
}

const ADD_ONE = adder(1);
const DATA = load();
const handlers: Handler[] = [];

function main() {
  handlers.push(ADD_ONE);
  console.log(handlers.indexOf(ADD_ONE), ADD_ONE === ADD_ONE, DATA);   // 0 true data
}
```

A panic (an integer overflow,
an index out of bounds) in an initializer happens at the use, where TypeScript would throw at
module load.
