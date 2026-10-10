# Built-ins

These names are in scope in every module without an import. They come from the compiler and from
the standard library's prelude ([prelude reference](../std/prelude.md)).

| Name | What it is |
|---|---|
| `console.log(a, b, …)`, `console.error(…)` | print the arguments separated by spaces to stdout / stderr. Values print like Node: `[ 1, 2 ]`, `{ k: 1, s: 'a' }`, `Map(1) { 'a' => 1 }`, `ClassName { field: value }`, a `JsonValue` like the parsed object, a promise as `Promise { <pending> }`, `Promise { 42 }` or `Promise { <rejected> … }` (its state now, without awaiting it or taking its value; unlike Node, a promise created outside an async function has not run yet, and one settled by `resolve`/`reject` shows it only once the task next waits, so both still print `<pending>`), numbers JS-style; an object graph that refers back to itself prints `<ref *1> Node { next: [Circular *1] }`, numbered per argument like Node (`[ <ref *1> …, <ref *2> … ]` for two cycles in one array). A value too long for one line (node's `breakLength` of 80 columns) prints one entry per line, indented by two spaces, and an array of more than six short entries in aligned columns, as Node does. Node's default limits apply: a container nested more than two levels deep prints as `[Object]`, `[Array]`, `[ClassName]`, `[Map]`, `[Set]` or `[Promise]` (an empty one in full, `{}` or `[]`; a reference back to an object being printed stays `[Circular *1]`), and an array, `Map` or `Set` prints its first 100 entries followed by `... 50 more items` (objects print every field) |
| `process.exit(code: i32)` | exit immediately |
| `process.stdout.write(s)`, `process.stderr.write(s)` | write a string without a newline, ordered with `console.log` / `console.error`; return `true` like Node |
| `process.env.NAME`, `process.env[name]` | an environment variable as `string \| null` (`null` where Node has `undefined`); set one with `setEnv` and list them with `envAll()` of [`velt:process`](../std/process.md), which also has `cwd()` and byte writes |
| `process.argv` | Node's `[runtime, script, ...args]` (`process.argv.slice(2)` is the arguments); a new array each read, so changing it in place is an error ([`velt:process`](../std/process.md)) |
| `process.memoryUsage()` | `{ rss, heapUsed }` in bytes, a `MemoryUsage` ([`velt:process`](../std/process.md#memory-usage)) |
| `panic(msg)` | stop with a panic (exit code 101) |
| `assert(cond, msg)`, `assertEq(a, b)` | panic when the check fails (`assertEq` compares contents) |
| `assertThrows(() => f())` | the error `f` throws; panics if it returns normally |
| `deepEqual(a, b)` | content comparison (`==` compares objects by identity) |
| `attempt(() => f())` | a throwing call as a value: `T \| E` ([Errors](errors.md#errors-as-values)) |
| `Math` | `PI`, `E`, `sqrt floor ceil round trunc abs sign max min pow hypot random`, … |
| `String(x)`, `Number(x)`, `Boolean(x)`, `parseInt(s, radix)`, `parseFloat(s)`, `isNaN(x)`, `isFinite(x)`, `String.fromCharCode(c)`, `NaN`, `Infinity` | conversions and checks |
| `Buffer.alloc(n)`, `Buffer.byteLength(s)` | byte arrays (`u8[]`) ([prelude](../std/prelude.md)) |
| `Number.isInteger(x)`, `Number.isNaN`, `isFinite`, `isSafeInteger`, `parseInt`, `parseFloat`, `Number.MAX_SAFE_INTEGER`, `MIN_SAFE_INTEGER`, `EPSILON`, `MAX_VALUE`, `MIN_VALUE`, `NaN`, `POSITIVE_INFINITY`, `NEGATIVE_INFINITY` | JS's `Number` members, on `f64` |
| `JSON.stringify`, `JSON.parse<T>`, `JSON.parseValue`, `JsonValue`, `JsonError` | JSON ([`velt:json`](../std/json.md)) |
| `Error` | base class of thrown errors: `class Error { message: string }` |
| `Comparable<T>` | ordering interface ([Comparable](classes.md#comparable)) |
| `Map<K, V>` | insertion-ordered hash map ([Types](types.md#objects-arrays-tuples-and-maps)) |
| `sleep(ms)`, `yieldNow()`, `spawn(p)`, `Promise.all/race/allSettled/any/withResolvers`, `new Promise((resolve, reject) => …)` | async ([Async](async.md)) |
| `setTimeout(task, ms)`, `clearTimeout(t)`, `setInterval(task, ms)`, `clearInterval(t)`, `Timer` | timers, as in TypeScript; the callback may return a promise (`() => save(doc)`, `async () => { … }`) or nothing (`() => console.log("x")`) ([velt:timers](../std/timers.md)) |
| `shared(x)`, `shared<T>`, `Mutex<T>` | thread-safe shared state ([Async](async.md#thread-safety)) |
| `performance.now(): f64`, `Date.now(): i64` | monotonic and wall-clock milliseconds |
| `Date` | JavaScript's dates ([prelude](../std/prelude.md#date)) |
| `fetch(input, init)`, `Request`, `Response`, `Headers` | the WHATWG Fetch API, as in Node ([fetch](../std/fetch.md)) |
| `AbortController`, `AbortSignal` | cancellation, e.g. of a `fetch` ([velt:task](../std/task.md)) |
| `URL`, `URLSearchParams` | WHATWG URLs ([velt:url](../std/url.md)) |
| `Set<T>` | insertion-ordered hash set ([velt:collections/set](../std/collections/set.md)) |
| `RegExp`, `/ab+c/gi` | regular expressions ([velt:regex](../std/regex.md)) |
| `TextEncoder`, `TextDecoder` | UTF-8 text to bytes and back ([velt:encoding](../std/encoding.md)) |
| `structuredClone(x)` | an independent deep copy, `x.clone()` ([Memory model](memory.md)); a function (at any depth) or an instance of a class of your own is a compile error, as JS throws or drops the class: write `x.clone()` |
| `Symbol.dispose`, `Symbol.asyncDispose` | cleanup method names ([Memory model](memory.md#resource-cleanup-using-and-symboldispose)) |
| `Symbol.iterator`, `Symbol.asyncIterator` | iteration method names ([Control flow](control-flow.md#iterables)) |

The modules behind `fetch`, `AbortController`, `URL`, `Set`, `RegExp` and `TextEncoder`
(and the names next to them) are loaded only by programs that mention one of those names (or
hold a regex literal), so the others don't pay for compiling them; a module that imports or
declares such a name itself (`import { Response } from "./api"`) uses its own and
doesn't load the global. Integer helpers (`gcd`,
`clamp`, …) are in [`velt:math`](../std/math.md). Everything else is imported from the
[standard library](../std/README.md).
