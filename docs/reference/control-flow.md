# Control flow

## Statements

- `if` / `else if` / `else`, `while`, `do … while`, C-style `for` (comma lists allowed:
  `for (let i = 0, j = n; i < j; i++, j--)`), `for (const x of xs)`, `break` and `continue`
  (optionally labeled: `outer: for (…)` … `continue outer;`), `return`, blocks, and the
  ternary `?:`. A body without braces (`if (c) return x;`) is a one-statement block.
- Conditions take `bool` or nullable values ([safe truthiness](variables.md#conditions-safe-truthiness)).

## `for...of`

- `for...of` iterates arrays, maps (`[key, value]` pairs), and classes with an `entries()`
  method.
- The loop variable is each element itself (objects are references): you can call its methods
  (including modifying ones), assign its fields, and store it elsewhere, which shares the
  element. Index the array to replace an element.
- Iterating a temporary (a call result, `await …`, a literal) **consumes** it: each element is
  handed to the loop variable without a count.
- There is no `for...in`; iterate `map.keys()` or an object's known fields.

## `switch`

`switch` has JavaScript semantics: the discriminant is evaluated once; the first `case` whose
value equals it is entered (`default` when none does, wherever it is written); bodies fall
through until `break`, `return`, `throw` or `continue`. `continue` cannot target a `switch`; a
labeled `switch` can be left with `break label` from nested loops. Each case body is its own
block scope.

- **Case values**: literals (numbers, also negative ones; strings; bools), `null`, enum members,
  or any expression of the discriminant's type (compared with `==`). Duplicates are an error.
- **Narrowing**: `switch (x.kind)` on a discriminated union narrows `x` in each case (a case
  reached by fallthrough sees the union of the members that can get there); `switch (typeof x)`
  narrows like `typeof` tests; `switch (x)` on a union narrows by literal member and by
  `case null`.
- **Exhaustiveness**: without `default`, a `switch` on a discriminant, a `typeof`, a union of
  literals or an enum must cover every possible member. A missing one is an error listing the
  cases (``missing cases: "rect", "tri"``), and a complete one needs no code after it. In
  `default`, the value is narrowed to the members no case took; with none left it is `never`,
  so `const _x: never = s;` checks exhaustiveness the TypeScript way.
- **Performance**: cases on tags, enums and integers dispatch through one jump table; string
  cases compare in order.

```ts
enum Level { Debug, Info, Warn }

function describe(n: i64): string {
  let s = "";
  switch (n) {
    case 1:
      s += "one ";              // falls through
    case 2:
      s += "two";
      break;
    default:
      s = "other";
  }
  return s;
}

function tag(l: Level): string {
  switch (l) {
    case Level.Debug:
      return "D";
    case Level.Info:
      return "I";
    case Level.Warn:
      return "W";
  }
}

function label(x: string | null): string {
  switch (x) {
    case null:
      return "none";
    default:
      return x;                 // x is a string here
  }
}

outer: for (let i = 0; i < 3; i++) {
  for (let j = 0; j < 3; j++) {
    if (j == 1) {
      continue outer;
    }
    console.log(i, j, describe(i), tag(Level.Info), label(null));
  }
}
```
