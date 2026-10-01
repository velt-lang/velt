# Built-ins

These names are in scope in every module without an import. They come from the compiler and from
the standard library's prelude ([prelude reference](../std/prelude.md)).

| Name | What it is |
|---|---|
| `console.log(a, b, …)`, `console.error(…)` | print the arguments separated by spaces to stdout / stderr. Values print like Node: `[ 1, 2 ]`, `{ k: 1, s: 'a' }`, `Map(1) { 'a' => 1 }`, `ClassName { field: value }`, numbers JS-style |
| `process.exit(code: i32)` | exit immediately |
| `panic(msg)` | stop with a panic (exit code 101) |
| `assert(cond, msg)`, `assertEq(a, b)` | panic when the check fails |
| `attempt(() => f())` | a throwing call as a value: `T \| E` ([Errors](errors.md#errors-as-values)) |
| `Math` | `PI`, `E`, `sqrt floor ceil round trunc abs sign max min pow hypot`, … |
| `Number(s)`, `parseInt(s, radix)`, `parseFloat(s)`, `String.fromCharCode(c)`, `NaN`, `Infinity` | conversions |
| `JSON.stringify`, `JSON.parse<T>`, `JSON.parseValue`, `JsonValue`, `JsonError` | JSON ([`velt:json`](../std/json.md)) |
| `Error` | base class of thrown errors: `class Error { message: string }` |
| `Comparable<T>` | ordering interface ([Comparable](classes.md#comparable)) |
| `Map<K, V>` | insertion-ordered hash map ([Types](types.md#objects-arrays-tuples-and-maps)) |
| `sleep(ms)`, `yieldNow()`, `spawn(p)`, `Promise.all/race/allSettled/any` | async ([Async](async.md)) |
| `shared(x)`, `shared<T>`, `Mutex<T>` | thread-safe shared state ([Async](async.md#thread-safety)) |
| `performance.now(): f64`, `Date.now(): i64` | monotonic and wall-clock milliseconds |
| `Symbol.dispose`, `Symbol.asyncDispose` | cleanup method names ([Memory model](memory.md#resource-cleanup-using-and-symboldispose)) |

Integer helpers (`gcd`, `clamp`, …) are in [`velt:math`](../std/math.md). Everything else is
imported from the [standard library](../std/README.md).
