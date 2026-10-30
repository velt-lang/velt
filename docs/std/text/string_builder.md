# velt/text: StringBuilder

`StringBuilder` is a global (no import; the module is `velt/text/string_builder`). It is Velt's own:
JavaScript has none. It builds a string by appending to one buffer that grows by doubling, so
`n` appends cost O(total length), and `take()` hands that buffer over without a copy.
`+=` on a string variable and template literals compile to the same builder, so a loop with
`s += x` is as fast. Use `StringBuilder` when the text is built across functions, or to append
numbers and code points without making a string for each.

```ts
const b = new StringBuilder();
for (let i: i64 = 0; i < 3; i++) {
  if (i > 0) {
    b.append(", ");
  }
  b.append("item ");
  b.appendInt(i);
}
b.appendCodePoint(0x2713);
console.log(b.length); // 23
console.log(b.toString()); // item 0, item 1, item 2✓
console.log(`${b}`.length, b.take().length, b.length); // 23 23 0
```

| Member | |
|---|---|
| `new StringBuilder(capacity = 0)` | an empty builder; `capacity` is the bytes it has room for before it grows (a hint) |
| `append(s: string)` | appends `s` |
| `appendCodePoint(code)` | appends the character `String.fromCodePoint(code)` makes (a code point, or a UTF-16 code unit, a lone surrogate included); a RangeError panic for a value that is no code point |
| `appendInt(n: i64)` | appends the decimal digits of `n`; a `number` must be converted first (`Math.trunc(x) as i64`), since a fraction can't be written as an integer |
| `appendNumber(x: f64)` | appends `x` as `String(x)` writes it (`1e+21`, `NaN`, `-0` as `0`) |
| `length: i64` | the length of the text so far in UTF-16 code units, as `toString().length` |
| `toString(): string` | the text so far; the builder keeps it. Shared, not copied: the next append copies it once |
| `take(): string` | the text, handed over without a copy; the builder is empty again and can build another string |
| `clear()` | empties the builder |

Converting the builder to a string (a template literal `${b}`, `+` with a string, `String(b)`)
calls `toString()`, so it leaves the builder as it was. Use `take()` for the last read of a large
text, to skip the one copy the next append would make. `length` is an integer (`i64`), as
Velt's own APIs give.
