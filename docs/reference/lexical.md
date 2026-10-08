# Lexical structure

## Programs

A program is a set of `.vlt` files. The root file defines `function main()` or
`async function main()`. `main` returns `void` (exit code 0) or `i32` (the exit code).
Declarations are hoisted: a function can be called above its definition.

```ts
function main(): i32 {
  console.log(greeting("world"));
  return 0;
}

function greeting(name: string): string {
  return `hello, ${name}`;
}
```

Source files are UTF-8. Each file is a module ([Modules](modules.md)).

## Comments and semicolons

- Comments: `// line` and `/* block */`.
- Doc comments: a JSDoc `/** … */` comment, or a block of `///` lines, that ends on the line
  right above a declaration documents it. The text is Markdown, with JSDoc tags (`@param`,
  `@returns`, `@throws`, `@example`, `@deprecated`, `@see`, `{@link name}`). A plain `//` or
  `/* */` comment is not documentation. Plain comments on the lines between a doc comment and
  its declaration (`// eslint-disable-next-line`, `// @ts-expect-error`) are skipped, as in
  TypeScript, but a blank line between them ends the association (TypeScript allows blank
  lines). The comment block at the very top of a file, followed by a blank line, documents the
  module (any comment style). `velt doc` and [the editor](../tooling/editors.md#features) show
  doc comments.

```ts
/**
 * The larger of `a` and `b`.
 *
 * @param a - the first number
 * @param b - the second number
 * @returns `a` when the two are equal
 */
export function larger(a: i64, b: i64): i64 {
  return a >= b ? a : b;
}
```

- Semicolons are required after statements. There is no automatic semicolon insertion.

## Identifiers

`[A-Za-z_$][A-Za-z0-9_$]*`, ASCII only.

Velt code is strict-mode code, as in TypeScript modules: no function, class, enum, variable,
parameter or import may be named `arguments` or `eval` (properties and methods may).

## Literals

- **Numbers**: `123`, `1_000_000`, `0xff`, `0b1010`, `0o17`, `1.5`, `1e21`, `2.5e-3`, with an
  optional type suffix: `10u8`, `5i32`, `1.0f32`. There are no `BigInt` literals (`10n`); use
  [`velt:bigint`](../std/bigint.md).
- **Strings**: `"..."` or `'...'`, with the escapes `\n \r \t \\ \" \' \0 \xHH \u{HHHH}`.
- **Template literals**: `` `a ${expr} b` `` may span lines and also escape `` \` `` and `\$`.
  `${expr}` formats any value the way `console.log` does.
- **Regular expressions**: `/ab+c/gi` is `new RegExp("ab+c", "gi")` from
  [`velt:regex`](../std/regex.md) (import `RegExp`). A `/` after an operand is division.
- `true`, `false`, `null`.

## Keywords

```
as async await break case catch class const constructor continue declare default do else
enum export extends false finally for from function if implements import in instanceof
interface let new null of readonly return shared static struct switch this throw true try
type typeof void while
```

Contextual keywords: `extend`, `get`, `set`, `override`, `private`, `public`, `throws`, `using`.

Not part of the language: `var`, `undefined`, `any`, `unknown`, `abstract`, `protected` (as a
member modifier; accepted on constructors and constructor parameter properties), `delete` (except
`delete r[k]` on a [`Record`](types.md#objects-arrays-tuples-and-maps)), `for...in`,
`export default`, `function` expressions (use arrow functions), `mut`, `match`. Writing most of
these is an error that names the Velt replacement.

## Operators

From highest to lowest precedence, with JavaScript's associativity:

| Operators | Notes |
|---|---|
| `x++` `x--` | postfix |
| `.` `?.` `()` `[]` | member access, optional chaining, call, index |
| `!` `~` `-` `+` `++x` `--x` `await` `typeof` | prefix |
| `**` | right-associative |
| `*` `/` `%` | |
| `+` `-` | |
| `<<` `>>` `>>>` | |
| `<` `<=` `>` `>=` `as` `instanceof` | |
| `==` `!=` `===` `!==` | `==` is `===` ([Types](types.md#equality-and-comparison)) |
| `&` | |
| `^` | |
| `\|` | |
| `&&` | |
| `\|\|` `??` | |
| `?:` | |
| `=` `+=` `-=` `*=` `/=` `%=` `**=` `<<=` `>>=` `>>>=` `&=` `\|=` `^=` `&&=` `\|\|=` `??=` | assignment |
| `=>` | arrow function |

The logical assignments `&&=`, `||=` and `??=` assign when `&&`, `||` or `??` would take their
right side: `x ??= d` is `x = x ?? d` ([Types](types.md#null)). As in JavaScript, a compound
assignment or `++` / `--` evaluates its target's object and indices once, before the right
side: `rows[next()].out += "a"` and `f().count++` call `next` and `f` once. The write goes to
that element where it is after the right side ran: `xs[0] += grow(xs)` updates `xs[0]` even when
`grow` made `xs` longer. If the right side made the array shorter than the index, that write
is out of bounds and panics, where JavaScript extends the array: `xs[0] += xs.pop()!` on `[5]`
panics, on `[5, 6]` it makes `[11]` (Velt arrays never grow by assignment, see
[Types](types.md#objects-arrays-tuples-and-maps)). A postfix `!` after an expression on the same
line is the non-null assertion (`m.get(k)!`).
