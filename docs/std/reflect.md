# velt/reflect

`import { typeOf, entryCount, entryName, entryValue } from "velt/reflect"`. What a dynamic value
(`any`, `unknown`) holds at run time, read from its type descriptor: its kind and type, and
the entries it lists (the fields of an object, the elements of an array, the entries of a
`Map`). It is for libraries that walk values whose type they do not know: a serializer, a
pretty-printer, a deep comparison. Std's own `JSON.stringify` of an `any` is written with it.

Values of known types don't need it: a [source generator](../reference/source-generators.md)
writes code per type at compile time, which is faster and checked. Reflection is for what
only the run time knows.

## What a value holds

- `typeOf(v: any): RuntimeType`, with
  - `kind`: `"undefined"`, `"null"`, `"boolean"`, `"number"`, `"string"`, `"symbol"`,
    `"function"`, `"array"`, `"tuple"`, `"object"` (an object type or literal), `"class"`,
    `"struct"`, `"map"`, `"record"`, `"set"`, `"shared"`, `"proxy"`, `"interface"`, `"enum"`
    or `"opaque"` (a value whose structure is not shown, such as a promise);
  - `repr`: the number type of a number (`"f64"`, `"i64"`, `"u8"`, …), `""` otherwise;
  - `name`: the type's name as messages show it (`"Point"`, `"Map"`, `"number"`, `"Object"`);
  - `hooks`: the hook methods its class has, of `toJSON`, `toString` and `__inspect`, and
    `hasHook(name)`.
- `kindOf(v: any): string`: `typeOf(v).kind` alone.

A class instance is described by its own class, also when it was converted to `any` from a
base class type.

## Entries

- `entryCount(v: any): i64`: how many entries `v` lists, or -1 when it cannot list them (a
  proxy, a `Map` whose keys are not strings, an object with a field no `any` can hold):
  - an object, a class instance or a struct: its fields, in JavaScript's key order (array index
    names first), without ES private fields (`#x`), symbol keys and functions; `private`
    fields are listed. An object literal's getter is listed with the fields, and reading it
    runs the getter;
  - an array or a tuple: its elements (named `""`);
  - a `Map<string, V>` or a `Record<string, V>`: its entry positions in insertion order; a
    deleted entry is listed with the name `""` and the value `undefined`;
  - a `shared` cell: the value it holds;
  - any other value: none (0).
- `entryName(v: any, i: i64): string` and `entryValue(v: any, i: i64): any`: entry `i`'s name
  and value. An absent optional field (`label?: string`), a deleted entry and an `i` out of
  range read as `undefined`; a field `n?: number | null` that holds `null` is `null`.
- `callToJSON(v: any, key: string): any`: what the class's `toJSON(key)` returns
  (`undefined` when it has none).

```ts
import { entryCount, entryName, entryValue, kindOf, typeOf } from "velt/reflect";

class Point {
  x: number = 1;
  label?: string;
  #secret: number = 7;
}

function main() {
  const v: any = new Point();
  console.log(typeOf(v).kind, typeOf(v).name, entryCount(v)); // class Point 2
  for (let i: i64 = 0; i < entryCount(v); i++) {
    console.log(entryName(v, i), kindOf(entryValue(v, i))); // x number, then label undefined
  }
}
```

## Identity and values that contain themselves

- `identity<T>(v: T): u64`: the same number for the same object, array or function, also
  when one is held by an `any`; 0 for a value without an identity (a number, a string, a
  struct held by value).
- `beginVisit(id: u64): bool` and `endVisit()`: a stack of the values being visited, per
  thread. `beginVisit` is false when `id` is being visited already, so the value contains
  itself (Node's "Converting circular structure to JSON"); each `true` needs an `endVisit`.
- `isProxy<T>(v: T): bool`: whether `v`, of a class or object type, is a typed `Proxy`, which
  a walk should treat as the `any` it converts to.

```ts
import { beginVisit, endVisit, identity } from "velt/reflect";

class Node {
  next: Node | null = null;
}

function depth(n: Node): i64 {
  if (!beginVisit(identity(n))) {
    return -1; // a cycle
  }
  const d = n.next == null ? 1 : depth(n.next!);
  endVisit();
  return d < 0 ? d : d + 1;
}

function main() {
  const a = new Node();
  a.next = new Node();
  console.log(depth(a)); // 3
  a.next!.next = a;
  console.log(depth(a)); // -1
}
```
