# Symbol

JavaScript's `Symbol`, a global (`std/symbol.vlt`): unique values compared by identity, which
also name members of objects ([Symbols](../reference/types.md#symbols)).

| Member | Meaning |
|---|---|
| `Symbol(description?)` | a new symbol, different from every other one |
| `Symbol.for(key)` | the registry's symbol for `key`, the same on every call |
| `Symbol.keyFor(s)` | the key of a symbol `Symbol.for` made, else `null` (`undefined` in Node) |
| `s.description` | the description given to `Symbol(...)`, or `null` (`undefined` in Node) |
| `s.toString()`, `String(s)` | `Symbol(description)` |
| `Symbol.iterator`, `Symbol.asyncIterator`, `Symbol.dispose`, `Symbol.asyncDispose`, `Symbol.toStringTag`, `Symbol.hasInstance`, `Symbol.toPrimitive`, `Symbol.species`, `Symbol.isConcatSpreadable`, `Symbol.match`, `Symbol.matchAll`, `Symbol.replace`, `Symbol.search`, `Symbol.split`, `Symbol.unscopables` | the well-known symbols |

```ts
const KEY = Symbol("key");

function main() {
  const a = Symbol("a");
  console.log(a, a === Symbol("a"), a.description, typeof a); // Symbol(a) false a symbol
  console.log(Symbol.for("k") === Symbol.for("k"), Symbol.keyFor(Symbol.for("k"))); // true k
  const seen = new Map<symbol, number>();
  seen.set(KEY, 1);
  console.log(seen.get(KEY), String(Symbol.iterator)); // 1 Symbol(Symbol.iterator)
}
```

A module constant initialized with `Symbol("...")` is made once, when the program is compiled,
so every use of the constant is the same symbol. `Symbol(...)` called at run time makes a new
symbol that lives until the program ends, and `Symbol.for` keeps one per key.
