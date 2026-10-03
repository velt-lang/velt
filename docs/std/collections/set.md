# velt:collections/set

`import { Set } from "velt:collections/set"`. An insertion-ordered hash set with JS `Set`
semantics plus the ES2025 set algebra. It is built on the prelude `Map`, so elements can be
anything a map key can be: numbers, bool, string, class instances (by identity), and structs,
object types and tuples (by content).

- `new Set<T>()`, `new Set(values)` (leaves `values` as it is and shares the elements, like JS),
  `Set.from(xs)` (clones the elements), `size`, `isEmpty()`.
- `add(v)`: takes ownership. `has(v)`, `delete(v): bool`, `clear()`.
- `values(): T[]` returns clones in insertion order. `forEach(f)` borrows.
- A set is an `Iterable<T>`: `for (const x of s)` visits its elements in insertion order, and it
  converts to an `Iterable<T>` value. `s[Symbol.iterator]()` iterates the elements as of the
  call (`values()`; JS's set iterator is a live view).
- Set algebra, each returning a new set: `union`, `intersection`, `difference`,
  `symmetricDifference`.
- Tests: `isSubsetOf`, `isSupersetOf`, `isDisjointFrom`.

```ts
import { Set } from "velt:collections/set";

function main() {
  const seen = new Set(["a", "b", "a", "c"]);
  seen.add("d");
  const other = Set.from(["c", "d", "e"]);
  console.log(seen.size, seen.has("b"), seen.values());
  console.log(seen.intersection(other).values(), seen.difference(other).values());
  console.log(seen.union(other).size, other.isSubsetOf(seen), seen.delete("a"));
}
```
