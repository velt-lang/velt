# velt:bigint

`import { BigInt, BigIntError } from "velt:bigint"`. Arbitrary-precision integers, like JS
`BigInt`, backed by the runtime (num-bigint). Velt has no operator overloading and no `10n`
literals, so arithmetic is methods.

- `new BigInt(value: i64 = 0)`, `BigInt.fromNumber(x: f64)` (the integer part; panics on `NaN`
  and ±Infinity), `BigInt.parse(s, radix = 10)` (an optional sign, then digits; throws
  `BigIntError`).
- Arithmetic returning a new value, like `a + b`: `add sub mul div rem` (with a `BigInt`),
  `addInt subInt mulInt divInt remInt` (with an `i64`), `shl(n)`, `shr(n)`, `neg()`. Division
  truncates and `rem` takes the dividend's sign, like JS; dividing by zero panics.
- In place, like `a += b`, reusing the buffer (use these in hot loops): `addAssign subAssign
  mulAssign divAssign remAssign`, `addIntAssign subIntAssign mulIntAssign divIntAssign`,
  `shlAssign`, `shrAssign`, and `set(other)`.
- Comparison: `compareTo(other)` (it implements `Comparable`), `compareToInt(k)`,
  `equals(other)`.
- Conversion: `toString(radix = 10)`, `toNumber(): f64`, `toI64()`.
- `clone()` copies the number. A `BigInt` owns a runtime handle that is freed when the value is
  dropped.

```ts
import { BigInt } from "velt:bigint";

function factorial(n: i64): BigInt {
  const acc = new BigInt(1);
  for (let i = 2; i <= n; i++) {
    acc.mulIntAssign(i);
  }
  return acc;
}

function main() {
  const f = factorial(25);
  console.log(f.toString()); // 15511210043330985984000000
  const big = BigInt.parse("ffffffffffffffff", 16);
  console.log(big.addInt(1).toString(16), f.compareTo(big) > 0); // 10000000000000000 true
}
```
