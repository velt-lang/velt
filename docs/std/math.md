# velt:math

`import { gcd } from "velt:math"`. Integer helpers. Float math is the prelude `Math` class.

- `clamp(x, lo, hi)`, `gcd(a, b)`, `lcm(a, b)`, `isPrime(n)`, `fib(n)`, all on `i64`.

```ts
import { clamp, gcd, lcm, isPrime, fib } from "velt:math";

function main() {
  console.log(clamp(15, 0, 10), gcd(12, 18), lcm(4, 6), isPrime(97), fib(50));
}
```
