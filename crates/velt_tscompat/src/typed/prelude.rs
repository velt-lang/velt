//! Which globals and members of the standard prelude (and the compiler's builtins) TypeScript
//! has with the same meaning, and which are Velt's own (`velt-global`, `velt-member`).
//!
//! The prelude's members are named by their owner: a class (`Date`, `Map`, `JSON`; static and
//! instance members alike), `Array`, `string`, `number` and `boolean` for the `extend` blocks
//! of those types, `nullable` for `extend<T> (T | null)`, `Number` for `NumberConstructor`. A
//! test reads `std/prelude` and fails on any export or member that isn't classified, so a new
//! one is decided on when it is added. The compiler's builtins (`spawn`, `console.log`,
//! `xs.push`) aren't in the prelude's source; they are listed here by hand.

/// A Velt-only global or member: its name and what to write in shared code instead.
pub(crate) type VeltOnly = (&'static str, &'static str);

/// Prelude exports and builtin globals that TypeScript's baseline has, with the same meaning.
/// (The lint reports the other half; this half is read by the classification test, and with
/// `TS_MEMBERS` by `scripts/gen-prelude-docs.js`, which seeds the prelude's doc comments from
/// TypeScript's.)
#[cfg(test)]
pub(crate) const TS_GLOBALS: &[&str] = &[
    "AggregateError",
    "ArrayIterator",
    "AsyncGenerator",
    "AsyncIterable",
    "AsyncIterableIterator",
    "AsyncIterator",
    "Date",
    "Error",
    "EvalError",
    "Generator",
    "Infinity",
    "Iterable",
    "IterableIterator",
    "Iterator",
    "IteratorObject",
    "IteratorResult",
    "JSON",
    "Map",
    "Math",
    "NaN",
    "Number",
    "Object",
    // What a proxy's `getOwnPropertyDescriptor` trap answers (data properties).
    "PropertyDescriptor",
    // `new Proxy(target, handler)` is an `any` in Velt for now (#823); the operations on it
    // run the handler's traps as in Node.
    "Proxy",
    "ProxyHandler",
    "RangeError",
    "ReferenceError",
    "Reflect",
    // TypeScript's `ReadonlyMap<K, V>` is a type: Velt's is `Map<K, V>` seen through it.
    "ReadonlyMap",
    // TypeScript's `Record<K, V>` is a type: as one it means the same.
    "Record",
    "String",
    "StringIterator",
    "SyntaxError",
    "TypeError",
    "URIError",
    "WeakMap",
    "WeakRef",
    "WeakSet",
    "isFinite",
    "isNaN",
    "parseFloat",
    "parseInt",
    "queueMicrotask",
    // The DOM's timer functions (TypeScript's baseline has the DOM's lib); in shared code the
    // handle only goes back to `clearTimeout` / `clearInterval` (a number in the browser).
    "clearInterval",
    "clearTimeout",
    "setInterval",
    "setTimeout",
    // A deep copy (TypeScript's baseline has it from the DOM's lib, Node as a global).
    "structuredClone",
    // TypeScript's dynamic types (prelude interfaces in Velt, std/prelude/z_dynamic.vlt).
    "any",
    "unknown",
    "object",
    "Function",
    // Builtins.
    "console",
    "Promise",
    "performance",
];

/// Velt-only globals: prelude exports and builtins.
pub(crate) const VELT_GLOBALS: &[VeltOnly] = &[
    (
        "Buffer",
        "pass bytes in as a `Uint8Array` or a `number[]` built outside the shared code",
    ),
    (
        "Comparable",
        "pass a comparator function instead: `(a: T, b: T) => number`",
    ),
    (
        "JsonError",
        "keep JSON decoding out of code shared with TypeScript",
    ),
    (
        "JsonParseOptions",
        "keep JSON decoding out of code shared with TypeScript",
    ),
    (
        "JsonValue",
        "keep dynamic JSON out of code shared with TypeScript, or pass typed values in",
    ),
    (
        "MemoryUsage",
        "keep process information out of code shared with TypeScript",
    ),
    (
        "ObjectPrototype",
        "compare prototypes with `===` without naming their type (`Object.getPrototypeOf(v) === Object.prototype`)",
    ),
    (
        "Mutex",
        "keep thread-shared state out of code shared with TypeScript",
    ),
    (
        "RwLock",
        "keep thread-shared state out of code shared with TypeScript",
    ),
    (
        "Atomic",
        "keep thread-shared state out of code shared with TypeScript",
    ),
    (
        "ThreadLocal",
        "keep thread-shared state out of code shared with TypeScript",
    ),
    (
        "AtomicValue",
        "keep thread-shared state out of code shared with TypeScript",
    ),
    (
        "Frozen",
        "keep thread-shared state out of code shared with TypeScript",
    ),
    ("NumberConstructor", "use `Number` (`Number.isInteger(x)`)"),
    (
        "StringBuilder",
        "collect the parts in a `string[]` and `join(\"\")` them, or append with `+=`",
    ),
    // TypeScript's has one type argument; Velt's two.
    (
        "PromiseSettledResult",
        "keep `Promise.allSettled` results out of shared types",
    ),
    (
        "PromiseWithResolvers",
        "keep it out of shared code: it is ES2024, past the baseline",
    ),
    (
        "Timer",
        "keep the timer handle's type out of shared code: in the browser `setTimeout` returns a number",
    ),
    (
        "assert",
        "throw an `Error` when the condition fails: `if (!c) throw new Error(msg)`",
    ),
    (
        "clearImmediate",
        "keep it out of shared code: it is Node's (the baseline has no Node types); use `clearTimeout`",
    ),
    (
        "setImmediate",
        "keep it out of shared code: it is Node's (the baseline has no Node types); use `setTimeout(f, 0)`",
    ),
    (
        "assertEq",
        "keep test assertions out of code shared with TypeScript",
    ),
    (
        "assertThrows",
        "keep test assertions out of code shared with TypeScript",
    ),
    (
        "AssertionError",
        "keep test assertions out of code shared with TypeScript (Node's is `node:assert`'s)",
    ),
    (
        "deepEqual",
        "compare the fields you need, or keep the comparison out of shared code",
    ),
    (
        "processMemoryUsage",
        "keep process information out of code shared with TypeScript",
    ),
    ("promiseAllSettled", "use `Promise.allSettled(…)`"),
    ("promiseAny", "use `Promise.any(…)`"),
    ("promiseCatch", "use `p.catch(…)`"),
    ("promiseCatchAsync", "use `p.catch(…)`"),
    ("promiseCatchVoid", "use `p.catch(…)`"),
    ("promiseCatchNever", "use `p.catch(…)`"),
    ("promiseCatchNeverAsync", "use `p.catch(…)`"),
    ("promiseCatchVoidAsync", "use `p.catch(…)`"),
    ("promiseFinally", "use `p.finally(…)`"),
    ("promiseFinallyAsync", "use `p.finally(…)`"),
    ("promiseFinallyAsyncThrows", "use `p.finally(…)`"),
    ("promiseFinallyThrows", "use `p.finally(…)`"),
    ("promiseNew", "use `new Promise(…)`"),
    ("promiseNewResolveOnly", "use `new Promise(…)`"),
    ("promiseReject", "use `Promise.reject(…)`"),
    ("promiseResolve", "use `Promise.resolve(…)`"),
    ("promiseResolveVoid", "use `Promise.resolve()`"),
    ("promiseThen", "use `p.then(…)`"),
    ("promiseThenAsync", "use `p.then(…)`"),
    ("promiseThenAsyncThrows", "use `p.then(…)`"),
    ("promiseThenElse", "use `p.then(…)`"),
    ("promiseThenElseAsync", "use `p.then(…)`"),
    ("promiseThenThrows", "use `p.then(…)`"),
    (
        "promiseWithResolvers",
        "use `new Promise(…)` and keep its functions",
    ),
    // Builtins.
    (
        "attempt",
        "catch the error with `try { … } catch (e) { … }`",
    ),
    (
        "spawn",
        "keep tasks and threads out of code shared with TypeScript",
    ),
    (
        "typeIs",
        "keep compile-time type tests out of code shared with TypeScript",
    ),
    (
        "asType",
        "keep compile-time type tests out of code shared with TypeScript",
    ),
    (
        "shared",
        "keep thread-shared values out of code shared with TypeScript",
    ),
    ("panic", "throw an `Error`: `throw new Error(msg)`"),
    ("sleep", "keep timers out of code shared with TypeScript"),
    (
        "yieldNow",
        "keep task scheduling out of code shared with TypeScript",
    ),
    (
        "process",
        "the client has no `process` (the baseline has no Node types): pass what it \
                 provides in",
    ),
];

/// Members TypeScript's baseline has on the same owner, with the same meaning.
#[cfg(test)]
pub(crate) const TS_MEMBERS: &[(&str, &[&str])] = &[
    (
        "Array",
        &[
            "at",
            "concat",
            "copyWithin",
            "entries",
            "every",
            "fill",
            "filter",
            "find",
            "findIndex",
            "findLast",
            "findLastIndex",
            "flat",
            "forEach",
            "includes",
            "indexOf",
            "join",
            "lastIndexOf",
            "map",
            "reduce",
            "reverse",
            "slice",
            "some",
            "sort",
            "splice",
            "toReversed",
            "toSorted",
            "toSpliced",
            "toString",
            "with",
            // Builtins.
            "length",
            "push",
            "pop",
            "shift",
            "unshift",
        ],
    ),
    ("AsyncGenerator", &["next", "return"]),
    ("AsyncIterator", &["next", "return"]),
    ("Generator", &["next", "return", "throw"]),
    ("Iterator", &["next", "return"]),
    ("ArrayIterator", &["next"]),
    ("StringIterator", &["next"]),
    (
        "Date",
        &[
            "UTC",
            "getDate",
            "getDay",
            "getFullYear",
            "getHours",
            "getMilliseconds",
            "getMinutes",
            "getMonth",
            "getSeconds",
            "getTime",
            "getTimezoneOffset",
            "getUTCDate",
            "getUTCDay",
            "getUTCFullYear",
            "getUTCHours",
            "getUTCMilliseconds",
            "getUTCMinutes",
            "getUTCMonth",
            "getUTCSeconds",
            "now",
            "parse",
            "setDate",
            "setFullYear",
            "setHours",
            "setMilliseconds",
            "setMinutes",
            "setMonth",
            "setSeconds",
            "setTime",
            "setUTCDate",
            "setUTCFullYear",
            "setUTCHours",
            "setUTCMilliseconds",
            "setUTCMinutes",
            "setUTCMonth",
            "setUTCSeconds",
            "toDateString",
            "toISOString",
            "toJSON",
            "toLocaleDateString",
            "toLocaleString",
            "toLocaleTimeString",
            "toString",
            "toTimeString",
            "toUTCString",
            "valueOf",
        ],
    ),
    ("Error", &["message", "name", "stack", "toString"]),
    ("JSON", &["parse", "stringify"]),
    (
        "Map",
        &[
            "clear", "delete", "entries", "forEach", "get", "has", "keys", "set", "size", "values",
        ],
    ),
    (
        "Math",
        &[
            "E", "PI", "abs", "ceil", "clz32", "floor", "hypot", "imul", "max", "min", "pow",
            "random", "round", "sign", "sqrt", "trunc",
        ],
    ),
    (
        "Number",
        &[
            "EPSILON",
            "MAX_SAFE_INTEGER",
            "MAX_VALUE",
            "MIN_SAFE_INTEGER",
            "MIN_VALUE",
            "NEGATIVE_INFINITY",
            "NaN",
            "POSITIVE_INFINITY",
            "isFinite",
            "isInteger",
            "isNaN",
            "isSafeInteger",
            "parseFloat",
            "parseInt",
        ],
    ),
    ("Object", &["entries", "fromEntries", "keys", "values"]),
    // `Object.prototype.hasOwnProperty` read as a value: its `call` is `Function.call`.
    ("__HasOwnProperty", &["call"]),
    // An array method read through a proxy (`Reflect.get(array, "push")`): its name.
    ("__ArrayMethod", &["name"]),
    (
        "Reflect",
        &[
            "defineProperty",
            "deleteProperty",
            "get",
            "getOwnPropertyDescriptor",
            "has",
            "ownKeys",
            "set",
        ],
    ),
    ("String", &["fromCharCode", "fromCodePoint"]),
    ("WeakMap", &["delete", "get", "has", "set"]),
    ("WeakRef", &["deref"]),
    ("WeakSet", &["add", "delete", "has"]),
    ("number", &["toExponential", "toFixed", "toPrecision"]),
    (
        "string",
        &[
            "at",
            "charAt",
            "charCodeAt",
            "codePointAt",
            "endsWith",
            "includes",
            "indexOf",
            "lastIndexOf",
            "localeCompare",
            "padEnd",
            "padStart",
            "repeat",
            "replace",
            "replaceAll",
            "slice",
            "split",
            "startsWith",
            "substring",
            "toLowerCase",
            "toUpperCase",
            "trim",
            "trimEnd",
            "trimStart",
            // Builtins.
            "length",
        ],
    ),
];

/// Velt-only members, by owner.
/// The hint for every member of `Atomic`, which TypeScript has no counterpart of.
const ATOMIC: &str = "keep thread-shared state out of code shared with TypeScript";

pub(crate) const VELT_MEMBERS: &[(&str, &[VeltOnly])] = &[
    (
        "Atomic",
        &[
            ("add", ATOMIC),
            ("and", ATOMIC),
            ("compareExchange", ATOMIC),
            ("compareExchangeWeak", ATOMIC),
            ("exchange", ATOMIC),
            ("fence", ATOMIC),
            ("load", ATOMIC),
            ("max", ATOMIC),
            ("min", ATOMIC),
            ("or", ATOMIC),
            ("spinHint", ATOMIC),
            ("store", ATOMIC),
            ("sub", ATOMIC),
            ("xor", ATOMIC),
        ],
    ),
    ("ThreadLocal", &[("get", ATOMIC), ("set", ATOMIC)]),
    (
        "Array",
        &[
            ("isEmpty", "write `xs.length === 0`"),
            ("prefetch", "leave it out: it is only a hint"),
            ("reserve", "leave it out: arrays grow as needed"),
            (
                "rotateLeft",
                "move the element with `xs.splice(end - 1, 0, ...xs.splice(start, 1))`",
            ),
            (
                "rotateRight",
                "move the element with `xs.splice(start, 0, ...xs.splice(end - 1, 1))`",
            ),
            ("set", "assign the element: `xs[i] = v`"),
            ("swap", "swap with `[xs[i], xs[j]] = [xs[j], xs[i]]`"),
            (
                "swapIn",
                "swap with `[xss[a][i], xss[b][j]] = [xss[b][j], xss[a][i]]`",
            ),
            ("truncate", "remove the tail with `xs.splice(n)`"),
            (
                "utf8Equals",
                "compare `new TextDecoder().decode(bytes)` with the string",
            ),
            (
                "utf8EqualsIgnoreCase",
                "compare the decoded text's `toLowerCase()`",
            ),
            ("utf8Slice", "decode with `new TextDecoder().decode(bytes)`"),
            ("write", "use `TextEncoder`'s `encodeInto(s, bytes)`"),
            ("xorMask", "XOR the bytes in a loop"),
            // Builtins.
            ("clone", "copy with `[...xs]` or `xs.slice()`"),
        ],
    ),
    (
        "Buffer",
        &[
            ("alloc", "pass bytes in instead"),
            ("allocUnsafe", "pass bytes in instead"),
            (
                "byteLength",
                "keep byte counting out of shared code (`TextEncoder` is planned, #377 phase 4)",
            ),
        ],
    ),
    (
        "Comparable",
        &[("compareTo", "pass a comparator function instead")],
    ),
    (
        "Date",
        &[("compareTo", "compare `a.getTime()` with `b.getTime()`")],
    ),
    (
        "ObjectPrototype",
        &[
            (
                "toJSON",
                "compare prototypes with `===`; don't serialize them",
            ),
            (
                "toString",
                "compare prototypes with `===`; don't convert them",
            ),
        ],
    ),
    (
        "JSON",
        &[(
            "parseValue",
            "keep dynamic JSON out of code shared with TypeScript",
        )],
    ),
    (
        "JsonValue",
        &[
            ("array", JSON_VALUE),
            ("as", JSON_VALUE),
            ("asBool", JSON_VALUE),
            ("asNumber", JSON_VALUE),
            ("asString", JSON_VALUE),
            ("at", JSON_VALUE),
            ("clone", JSON_VALUE),
            ("delete", JSON_VALUE),
            ("from", JSON_VALUE),
            ("get", JSON_VALUE),
            ("has", JSON_VALUE),
            ("isArray", JSON_VALUE),
            ("isBool", JSON_VALUE),
            ("isNull", JSON_VALUE),
            ("isNumber", JSON_VALUE),
            ("isObject", JSON_VALUE),
            ("isString", JSON_VALUE),
            ("keys", JSON_VALUE),
            ("len", JSON_VALUE),
            ("object", JSON_VALUE),
            ("of", JSON_VALUE),
            ("parse", JSON_VALUE),
            ("push", JSON_VALUE),
            ("set", JSON_VALUE),
            ("setAt", JSON_VALUE),
            ("stringify", JSON_VALUE),
        ],
    ),
    (
        "Map",
        &[
            (
                "getOrInsert",
                "write `m.get(k) ?? d` and `m.set(k, d)` when it is missing",
            ),
            (
                "getUtf8",
                "decode the key first: `m.get(new TextDecoder().decode(bytes))`",
            ),
            (
                "hasUtf8",
                "decode the key first: `m.has(new TextDecoder().decode(bytes))`",
            ),
            (
                "setUtf8",
                "decode the key first: `m.set(new TextDecoder().decode(bytes), v)`",
            ),
            ("update", "read with `m.get(k)`, then `m.set(k, f(v))`"),
            ("upsert", "read with `m.get(k)`, then `m.set(k, …)`"),
        ],
    ),
    (
        "Promise",
        // Builtins.
        &[(
            "withResolvers",
            "use `new Promise(…)` and keep its functions: `Promise.withResolvers` is ES2024, \
             past the baseline",
        )],
    ),
    (
        "Math",
        &[(
            "umulh",
            "keep 128-bit arithmetic out of code shared with TypeScript",
        )],
    ),
    (
        "Timer",
        &[
            ("clear", "call `clearTimeout(t)` or `clearInterval(t)`"),
            ("cleared", "keep track of it yourself"),
            ("hasRef", TIMER_REF),
            ("ref", TIMER_REF),
            ("unref", TIMER_REF),
            ("started", "keep track of it yourself"),
        ],
    ),
    ("boolean", &[("compareTo", COMPARE)]),
    (
        "number",
        &[
            ("compareTo", COMPARE),
            ("countOnes", "count the set bits in a loop"),
            (
                "fromBits",
                "read the bits through a `DataView` (`setBigUint64`, `getFloat64`)",
            ),
            ("leadingZeros", "use `Math.clz32(x)` (32 bits)"),
            (
                "toBits",
                "write the number to a `DataView` (`setFloat64`) and read `getBigUint64`",
            ),
            ("trailingZeros", "use `31 - Math.clz32(x & -x)` (32 bits)"),
        ],
    ),
    (
        "string",
        &[("compareTo", "compare with `<` or `a.localeCompare(b)`")],
    ),
    (
        "nullable",
        &[
            ("isNull", "write `x === null`"),
            ("map", "write `x === null ? null : f(x)`"),
            ("unwrap", "write `x!`"),
            ("unwrapOr", "write `x ?? d`"),
        ],
    ),
];

const JSON_VALUE: &str = "keep dynamic JSON out of code shared with TypeScript";
const COMPARE: &str = "compare with `<` and `>`, or subtract: `a - b`";
const TIMER_REF: &str =
    "keep it out of shared code: it is Node's, and in the browser `setTimeout` returns a number";

/// What to write instead of the Velt-only global `name`, if it is one.
pub(crate) fn velt_global(name: &str) -> Option<&'static str> {
    VELT_GLOBALS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, hint)| *hint)
}

/// What to write instead of the Velt-only member `name` of `owner`, if it is one.
pub(crate) fn velt_member(owner: &str, name: &str) -> Option<&'static str> {
    VELT_MEMBERS
        .iter()
        .filter(|(o, _)| *o == owner)
        .flat_map(|(_, members)| members.iter())
        .find(|(n, _)| *n == name)
        .map(|(_, hint)| *hint)
}

#[cfg(test)]
mod tests;
