# velt:random

`import { random, randomInt } from "velt:random"`. Fast pseudo-random numbers like
`Math.random()`, for simulations, sampling, shuffling, jitter and load generation. **Not** for
secrets or tokens: use velt:crypto's `randomBytes` / `randomInt` for those. Each thread has its
own generator (wyrand, seeded from the OS on first use), so concurrent tasks never contend and
every run gives a different sequence.

- `random(): f64`: uniform in `[0, 1)` (53 random bits).
- `randomInt(min, max): i64`: uniform in `[min, max)`; `min` when `max <= min`.

```ts
import { random, randomInt } from "velt:random";

function main() {
  const x = random();
  const die = randomInt(1, 7);
  console.log(x >= 0.0 && x < 1.0, die >= 1 && die < 7); // true true
}
```
