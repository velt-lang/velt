# Built-ins

These names are in scope in every module without an import. They come from the compiler and from
the standard library's prelude ([prelude reference](../std/prelude.md)).

| Name | What it is |
|---|---|
| `console.log(a, b, …)`, `console.error(…)` | print the arguments separated by spaces to stdout / stderr. Values print like Node: `[ 1, 2 ]`, `{ k: 1, s: 'a' }`, `Map(1) { 'a' => 1 }`, `ClassName { field: value }`, a `JsonValue` like the parsed object, a promise as `Promise { <pending> }`, `Promise { 42 }` or `Promise { <rejected> … }` (its state now, without awaiting it or taking its value; unlike Node, a promise created outside an async function has not run yet, and one settled by `resolve`/`reject` shows it only once the task next waits, so both still print `<pending>`), numbers JS-style; an object graph that refers back to itself prints `<ref *1> Node { next: [Circular *1] }` |
| `process.exit(code: i32)` | exit immediately |
| `process.stdout.write(s)`, `process.stderr.write(s)` | write a string without a newline, ordered with `console.log` / `console.error`; return `true` like Node |
| `process.env.NAME`, `process.env[name]` | an environment variable as `string \| null` (`null` where Node has `undefined`); set one with `setEnv` and list them with `envAll()` of [`velt:process`](../std/process.md), which also has `args()`, `cwd()` and byte writes |
| `process.memoryUsage()` | `{ rss, heapUsed }` in bytes, a `MemoryUsage` ([`velt:process`](../std/process.md#memory-usage)) |
| `panic(msg)` | stop with a panic (exit code 101) |
| `assert(cond, msg)`, `assertEq(a, b)` | panic when the check fails (`assertEq` compares contents) |
| `assertThrows(() => f())` | the error `f` throws; panics if it returns normally |
| `deepEqual(a, b)` | content comparison (`==` compares objects by identity) |
| `attempt(() => f())` | a throwing call as a value: `T \| E` ([Errors](errors.md#errors-as-values)) |
| `Math` | `PI`, `E`, `sqrt floor ceil round trunc abs sign max min pow hypot random`, … |
| `Number(s)`, `parseInt(s, radix)`, `parseFloat(s)`, `String.fromCharCode(c)`, `NaN`, `Infinity` | conversions |
| `Number.isInteger(x)`, `Number.isNaN`, `isFinite`, `isSafeInteger`, `parseInt`, `parseFloat`, `Number.MAX_SAFE_INTEGER`, `MIN_SAFE_INTEGER`, `EPSILON`, `MAX_VALUE`, `MIN_VALUE`, `NaN`, `POSITIVE_INFINITY`, `NEGATIVE_INFINITY` | JS's `Number` members, on `f64` |
| `JSON.stringify`, `JSON.parse<T>`, `JSON.parseValue`, `JsonValue`, `JsonError` | JSON ([`velt:json`](../std/json.md)) |
| `Error` | base class of thrown errors: `class Error { message: string }` |
| `Comparable<T>` | ordering interface ([Comparable](classes.md#comparable)) |
| `Map<K, V>` | insertion-ordered hash map ([Types](types.md#objects-arrays-tuples-and-maps)) |
| `sleep(ms)`, `yieldNow()`, `spawn(p)`, `Promise.all/race/allSettled/any/withResolvers` | async ([Async](async.md)) |
| `shared(x)`, `shared<T>`, `Mutex<T>` | thread-safe shared state ([Async](async.md#thread-safety)) |
| `performance.now(): f64`, `Date.now(): i64` | monotonic and wall-clock milliseconds |
| `Date` | JavaScript's dates ([prelude](../std/prelude.md#date)) |
| `Symbol.dispose`, `Symbol.asyncDispose` | cleanup method names ([Memory model](memory.md#resource-cleanup-using-and-symboldispose)) |
| `Symbol.iterator`, `Symbol.asyncIterator` | iteration method names ([Control flow](control-flow.md#iterables)) |

Integer helpers (`gcd`, `clamp`, …) are in [`velt:math`](../std/math.md). Everything else is
imported from the [standard library](../std/README.md).
