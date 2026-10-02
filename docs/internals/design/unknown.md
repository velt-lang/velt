# Design: `unknown` for dynamic JSON

Status: proposed (issue #18). Nothing here is implemented. The language has no `any` or `unknown`
([types](../../reference/types.md)); dynamic JSON is `JsonValue`, read through methods.

## Problem

TypeScript reads JSON of unknown shape with `unknown` and narrowing:

```ts ignore
const v: unknown = JSON.parse(text);
if (typeof v === "object" && v !== null && "items" in v && Array.isArray(v.items)) {
  for (const item of v.items) {
    if (typeof item === "string") console.log(item.toUpperCase());
  }
}
```

In Velt today the same code is a chain of method calls:
`v.get("items")?.isArray()`, `items.at(i)?.asString()`. That is correct but unfamiliar, and it
cannot be ported line by line. Typed `JSON.parse<T>` covers documents whose shape is known. This
design covers the rest.

## Proposal

**Scope.** `unknown` is the type of *a JSON value*:

- null, a boolean, a number, a string, an array of `unknown`, or an object of `unknown`;
- nothing else (no functions, class instances or promises).

That keeps one representation and no boxing anywhere else. A general top type is out of scope
(see "Not proposed").

**Representation.** An `unknown` is a `JsonValue` handle: a reference-counted, immutable tree
node, as today. Narrowing changes only the static type; it never converts the value. Reading a
narrowed value goes through the node:

| Test (TypeScript syntax) | `v` reads as, where it holds | Cost of a read |
|---|---|---|
| `typeof v === "string"` | `string` | copy of the string, like `asString()` |
| `typeof v === "number"` | `f64` | a load |
| `typeof v === "boolean"` | `bool` | a load |
| `v === null` / `v !== null` | `null` / still `unknown` | — |
| `Array.isArray(v)` | an array view: `v.length`, `v[i]: unknown`, `for (const x of v)` | no copy: indexes the node |
| `typeof v === "object" && v !== null` | an object view: `Object.keys(v)`, `"k" in v` | no copy |
| `"k" in v` (on an object view) | `v.k: unknown` | one key lookup (hashed past 16 keys) |

The tests combine with `&&`, `||`, `!` and early exits, like the existing narrowing of unions
and nullable values. A narrowed view is never copied into a Velt array or struct unless the code
asks: `JSON.parse<T>` or `v.as<T>()` (#19) decodes into a type.

**Where values come from.**

- `JSON.parseValue(text)` returns `unknown`, and `JSON.parse<unknown>(text)` is the same.
- A typed decoder field of type `unknown` holds any value.
- Assigning a value with a JSON form (`const u: unknown = { a: 1, b: [x] }`) builds a tree. That
  is the conversion `JsonValue.from(x)` does (#19), and it costs an encode.
- Anything without a JSON form is an error.

**Relationship to `JsonValue`.** The two are the same value, and converting between them is free
in both directions:

- `unknown` has no members until narrowed, as in TypeScript.
- `JsonValue` keeps its methods (`get`, `at`, `isString`, ...) and #19's mutators, for code that
  prefers them or needs to edit.
- `JSON.stringify` writes either.
- `JsonValue` stays a std type, not an alias of a language keyword, so existing code is
  unchanged.

## Compiler changes

- **Syntax:**
  - `unknown` becomes a built-in type name. Today it is just an unresolved name (`cannot find
    type`).
  - The binary operator `in` (`"k" in v`): `in` is lexed as a keyword but unused in expressions.
    This adds `BinaryOp::In` to `ast.rs`, a contract change.
- **HIR:** `TyKind::Unknown`, lowered like `JsonValue` (a handle). Reads of narrowed values are
  intrinsics over the existing runtime accessors (`velt_rt_json_value_as_str`, `_as_f64`,
  `_get`, `_at`, `_len`, `_key_at`); no new runtime representation is needed.
- **Sema narrowing** (`body/narrow.rs`): today a `Fact` refines a local to union members or to
  non-null. A third fact, `JsonKind(local, kind)`, records what `typeof`, `Array.isArray`,
  `=== null` and `in` established. A read of the local becomes the matching intrinsic. Field
  narrowing (`v.items` after `"items" in v`) reuses `field_narrow`'s path tokens.
- **Std:**
  - `Array.isArray(x): bool`, a prelude static; on a non-`unknown` argument it is a constant.
  - `Object.keys(v)` for object views, next to the `Record` overloads.

## Diagnostics

- Using an `unknown` before narrowing:
  `` `v` is `unknown`: check it first with `typeof`, `Array.isArray(v)` or `"key" in v` ``.
- `v.k` on an object view without `"k" in v`: `` `v` may not have a key "k": test `"k" in v` first ``.
- Assigning a value without a JSON form: the existing `has no JSON form` error.
- An impossible test (`typeof v === "function"`): the existing `typeof` "always false" error.

## Not proposed

- `unknown` as a general top type: a thrown value of any type, generic code over "anything".
  That needs a boxed universal representation and would slow code that never uses it. `catch`
  already gets an exact union of the thrown types.
- `any`.

## Open questions

1. Should `JSON.parseValue` keep returning `JsonValue`, with `unknown` coming only from
   annotations and `JSON.parse<unknown>`, or switch to `unknown` (TypeScript-like, but a change
   for existing code that calls methods on the result)?
2. Should assigning a typed value to `unknown` build a tree implicitly, or require
   `JsonValue.from(x)` so the cost is visible?
3. Should numbers in an `unknown` stay `f64` (what JavaScript does), or keep the integer text so
   that `JSON.parse<i64>` of a narrowed value stays exact past 2^53?
