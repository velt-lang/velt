# velt:mem

`import { needsDrop, Blank } from "velt:mem"`. Facts about how values are stored, for code that
manages its own memory layout, such as a container with slots for many values (Rust's
`std::mem`).

- `needsDrop(value)`: whether values of `value`'s type own memory that dropping one releases.
  Strings, arrays, objects and class instances do, and so does a struct or union holding one;
  numbers, `bool` and SIMD vectors do not. The answer is the same for every value of a type and
  is a constant wherever the type is known, so a branch on it costs nothing at run time. A
  generic container uses it to skip work that only matters for values that hold memory, such
  as releasing the last copy of a removed entry.
- `Blank<T>`, an interface every type has: `x.blank()` is a value of `x`'s type that owns no
  memory, for a container to keep in the slots it does not use (an array of plain `T` has no
  holes), so that a value it removes is released at once. A string's is `""`; a type whose
  values own no memory is its own blank. A type without a blank value gives the value itself,
  and `x.hasBlank()` is then `false`: the container keeps that value only while it is live.
  Implement it for a type of your own with an `extend` block
  (`extend Name implements Blank<Name> { hasBlank() { return true; } blank() { … } }`).

```ts
import { needsDrop } from "velt:mem";

class Point {
  x: f64 = 0;
}

function describe<T>(value: T): string {
  return needsDrop(value) ? "holds memory" : "plain";
}

function main() {
  console.log(describe(1), describe(true), describe("text"), describe([1, 2]), describe(new Point()));
  // plain plain holds memory holds memory holds memory
}
```

```ts
import { Blank } from "velt:mem";

function slots<T>(first: T, n: i64): T[] {
  const out: T[] = [first];
  for (let i = 1; i < n; i++) {
    out.push(first.blank());
  }
  return out;
}

function main() {
  console.log(slots("abc", 3), slots(7, 3), "abc".hasBlank());
  // [ 'abc', '', '' ] [ 7, 7, 7 ] true
}
```
