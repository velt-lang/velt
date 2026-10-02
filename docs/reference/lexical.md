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

- Comments: `// line` and `/* block */`. A `///` comment right above an exported declaration is
  its documentation (`velt doc`).
- Semicolons are required after statements. There is no automatic semicolon insertion.

## Identifiers

`[A-Za-z_$][A-Za-z0-9_$]*`, ASCII only.

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
| `=` `+=` `-=` `*=` `/=` `%=` `**=` `<<=` `>>=` `>>>=` `&=` `\|=` `^=` | assignment |
| `=>` | arrow function |

The logical assignments `&&=`, `||=` and `??=` parse but are rejected ("not supported yet").
